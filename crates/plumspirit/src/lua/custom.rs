//! Custom note tools (B1-0045 – B1-0048, `docs/LUA_API.md` "Custom tools"):
//! what a plugin declares with `plum.plugin.tool{}` — a stable id, a name,
//! a tooltip, a parameter schema PlumSpirit renders as a native form, the
//! selection it needs — and how a run reaches its `run(ctx)` function.
//!
//! A run is one transaction labelled with the tool's name, opened by the
//! host before `run` and committed after it: Apply is one Undo step, a
//! failure changes nothing, and a preview is the same run whose commits the
//! window keeps (Preview = Apply: Apply applies exactly what was shown).

use super::handles::{LuaHandle, current_world};
use super::plugin_api::{PluginSlot, TOOLS, check_decl_id, require_loading, text_field};
use super::{Category, stop};
use mlua::{Function, Lua, ObjectLike, Table, Value};
use plumspirit_engine::model::{ContentId, TrackId};
use plumspirit_engine::transform::param::Curve;

fn runtime(msg: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, msg)
}

/// Most parameters a custom tool may declare.
pub const MAX_PARAMS: usize = 64;
/// Most entries of a subchannel map.
pub const MAX_MAP: usize = 256;

/// A keyboard-and-velocity note filter (`note_filter` parameters).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteFilter {
    /// Inclusive key range (0 – 255; the project's keys bound it).
    pub keys: (u8, u8),
    /// Inclusive velocity range (1 – 127).
    pub velocity: (u8, u8),
    /// One MIDI channel (0 – 15) or every channel.
    pub channel: Option<u8>,
}

impl Default for NoteFilter {
    fn default() -> Self {
        NoteFilter {
            keys: (0, 255),
            velocity: (1, 127),
            channel: None,
        }
    }
}

/// What a parameter is (the form's control).
#[derive(Clone, Debug, PartialEq)]
pub enum ToolParamKind {
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
        unit: String,
    },
    Bool,
    Choice(Vec<String>),
    /// A key: 0 – 127, or 0 – 255 in a 256-key project.
    Pitch,
    /// A length in ticks.
    Ticks {
        min: u64,
        max: u64,
    },
    /// The random seed (dice button).
    Seed,
    /// An RGB color.
    Color,
    /// A graph on 0 – 1 → 0 – 1 (the native curve editor).
    Curve,
    /// An inclusive integer range.
    Range {
        min: i64,
        max: i64,
    },
    /// Subchannel → subchannel pairs.
    SubchannelMap,
    /// Keys, velocities and a channel.
    NoteFilter,
    Text {
        max_len: usize,
        multiline: bool,
    },
}

impl ToolParamKind {
    /// The name scripts use.
    pub fn name(&self) -> &'static str {
        match self {
            ToolParamKind::Int { .. } => "int",
            ToolParamKind::Float { .. } => "float",
            ToolParamKind::Bool => "bool",
            ToolParamKind::Choice(_) => "choice",
            ToolParamKind::Pitch => "pitch",
            ToolParamKind::Ticks { .. } => "ticks",
            ToolParamKind::Seed => "seed",
            ToolParamKind::Color => "color",
            ToolParamKind::Curve => "curve",
            ToolParamKind::Range { .. } => "range",
            ToolParamKind::SubchannelMap => "subchannel_map",
            ToolParamKind::NoteFilter => "note_filter",
            ToolParamKind::Text { .. } => "text",
        }
    }
}

/// A parameter's value.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Index into the choices.
    Choice(usize),
    Text(String),
    Color([u8; 3]),
    Curve(Curve),
    Range(i64, i64),
    Map(Vec<(u32, u32)>),
    Filter(NoteFilter),
}

/// One control of a custom tool.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolParam {
    /// Stable key (`a-z 0-9 _`, 1 – 32): presets and scripts use it.
    pub key: String,
    pub label: String,
    pub help: String,
    pub kind: ToolParamKind,
    pub default: ToolValue,
}

/// A custom note tool a plugin registered.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDecl {
    pub id: String,
    pub name: String,
    pub tooltip: String,
    pub params: Vec<ToolParam>,
    /// Apply needs selected notes (default); otherwise a run without a
    /// selection works on every note of the edited track / clip.
    pub needs_selection: bool,
    /// Optional host Piano Roll freehand capture; absent preserves API0.1 behavior.
    pub freehand: bool,
}

impl ToolDecl {
    pub fn defaults(&self) -> Vec<ToolValue> {
        self.params.iter().map(|p| p.default.clone()).collect()
    }
}

fn int_of(v: &Value, what: &str) -> mlua::Result<i64> {
    match v {
        Value::Integer(n) => Ok(*n),
        Value::Number(f) if f.fract() == 0.0 && f.abs() < 9.007e15 => Ok(*f as i64),
        other => Err(runtime(format!(
            "{what} must be a whole number, not {}.",
            other.type_name()
        ))),
    }
}

fn num_of(v: &Value, what: &str) -> mlua::Result<f64> {
    match v {
        Value::Integer(n) => Ok(*n as f64),
        Value::Number(f) if f.is_finite() => Ok(*f),
        other => Err(runtime(format!(
            "{what} must be a number, not {}.",
            other.type_name()
        ))),
    }
}

/// A `{lo, hi}` / `{ lo =, hi = }` pair.
fn pair_of(v: &Value, what: &str) -> mlua::Result<(i64, i64)> {
    let Value::Table(t) = v else {
        return Err(runtime(format!("{what} must be a pair {{ low, high }}.")));
    };
    let lo = match t.get::<Value>("lo")? {
        Value::Nil => t.raw_get::<Value>(1)?,
        v => v,
    };
    let hi = match t.get::<Value>("hi")? {
        Value::Nil => t.raw_get::<Value>(2)?,
        v => v,
    };
    let (lo, hi) = (int_of(&lo, what)?, int_of(&hi, what)?);
    if lo > hi {
        return Err(runtime(format!(
            "{what}: the low end is above the high end."
        )));
    }
    Ok((lo, hi))
}

/// A value of `kind` from a script value (`what` names it in errors).
pub fn value_from_lua(kind: &ToolParamKind, v: &Value, what: &str) -> mlua::Result<ToolValue> {
    Ok(match kind {
        ToolParamKind::Int { min, max } => {
            let n = int_of(v, what)?;
            if n < *min || n > *max {
                return Err(runtime(format!("{what} is {min} – {max} (it is {n}).")));
            }
            ToolValue::Int(n)
        }
        ToolParamKind::Float { min, max, .. } => {
            let f = num_of(v, what)?;
            if f < *min || f > *max {
                return Err(runtime(format!("{what} is {min} – {max} (it is {f}).")));
            }
            ToolValue::Float(f)
        }
        ToolParamKind::Bool => match v {
            Value::Boolean(b) => ToolValue::Bool(*b),
            other => {
                return Err(runtime(format!(
                    "{what} must be true or false, not {}.",
                    other.type_name()
                )));
            }
        },
        ToolParamKind::Choice(choices) => {
            let s = match v {
                Value::String(s) => s.to_string_lossy(),
                other => {
                    return Err(runtime(format!(
                        "{what} must be one of the choices, not {}.",
                        other.type_name()
                    )));
                }
            };
            let i = choices.iter().position(|c| *c == s).ok_or_else(|| {
                runtime(format!(
                    "{what} is \u{201C}{s}\u{201D}; the choices are {}.",
                    choices.join(", ")
                ))
            })?;
            ToolValue::Choice(i)
        }
        ToolParamKind::Pitch => {
            let n = int_of(v, what)?;
            if !(0..=255).contains(&n) {
                return Err(runtime(format!("{what} is a key 0 – 255 (it is {n}).")));
            }
            ToolValue::Int(n)
        }
        ToolParamKind::Ticks { min, max } => {
            let n = int_of(v, what)?;
            if n < 0 || (n as u64) < *min || (n as u64) > *max {
                return Err(runtime(format!(
                    "{what} is {min} – {max} ticks (it is {n})."
                )));
            }
            ToolValue::Int(n)
        }
        ToolParamKind::Seed => ToolValue::Int(int_of(v, what)?),
        ToolParamKind::Color => match super::clips::color_arg(v)? {
            Some(c) => ToolValue::Color(c),
            None => {
                return Err(runtime(format!(
                    "{what} must be a color {{ r =, g =, b = }}."
                )));
            }
        },
        ToolParamKind::Curve => {
            let Value::Table(t) = v else {
                return Err(runtime(format!(
                    "{what} must be a list of points {{ {{x, y}}, … }} with x and y 0 – 1."
                )));
            };
            let mut pts = Vec::new();
            for p in t.clone().sequence_values::<Table>() {
                let p = p?;
                let x = num_of(&p.raw_get::<Value>(1)?, what)?;
                let y = num_of(&p.raw_get::<Value>(2)?, what)?;
                if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
                    return Err(runtime(format!(
                        "{what}: every point is 0 – 1 on both axes."
                    )));
                }
                pts.push((x, y));
            }
            if pts.len() < 2 || pts.len() > plumspirit_engine::transform::param::MAX_CURVE_POINTS {
                return Err(runtime(format!(
                    "{what} needs 2 – {} points.",
                    plumspirit_engine::transform::param::MAX_CURVE_POINTS
                )));
            }
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            ToolValue::Curve(Curve::new(pts))
        }
        ToolParamKind::Range { min, max } => {
            let (lo, hi) = pair_of(v, what)?;
            if lo < *min || hi > *max {
                return Err(runtime(format!("{what} is within {min} – {max}.")));
            }
            ToolValue::Range(lo, hi)
        }
        ToolParamKind::SubchannelMap => {
            let Value::Table(t) = v else {
                return Err(runtime(format!(
                    "{what} must be a list of pairs {{ {{from, to}}, … }} (subchannel ids)."
                )));
            };
            let mut map = Vec::new();
            for p in t.clone().sequence_values::<Value>() {
                let (a, b) = pair_of_any(&p?, what)?;
                if a < 0 || b < 0 || a > i64::from(u32::MAX) || b > i64::from(u32::MAX) {
                    return Err(runtime(format!("{what}: subchannel ids are 0 or more.")));
                }
                map.push((a as u32, b as u32));
            }
            if map.len() > MAX_MAP {
                return Err(runtime(format!("{what} has more than {MAX_MAP} entries.")));
            }
            ToolValue::Map(map)
        }
        ToolParamKind::NoteFilter => {
            let Value::Table(t) = v else {
                return Err(runtime(format!(
                    "{what} must be {{ keys = {{lo, hi}}, velocity = {{lo, hi}}, channel = n }}."
                )));
            };
            let mut f = NoteFilter::default();
            if let v @ Value::Table(_) = t.get::<Value>("keys")? {
                let (lo, hi) = pair_of(&v, what)?;
                if lo < 0 || hi > 255 {
                    return Err(runtime(format!("{what}: keys are 0 – 255.")));
                }
                f.keys = (lo as u8, hi as u8);
            }
            if let v @ Value::Table(_) = t.get::<Value>("velocity")? {
                let (lo, hi) = pair_of(&v, what)?;
                if lo < 1 || hi > 127 {
                    return Err(runtime(format!("{what}: velocities are 1 – 127.")));
                }
                f.velocity = (lo as u8, hi as u8);
            }
            match t.get::<Value>("channel")? {
                Value::Nil => {}
                v => {
                    let c = int_of(&v, what)?;
                    if !(0..=15).contains(&c) {
                        return Err(runtime(format!("{what}: channel is 0 – 15.")));
                    }
                    f.channel = Some(c as u8);
                }
            }
            ToolValue::Filter(f)
        }
        ToolParamKind::Text { max_len, multiline } => {
            let s = match v {
                Value::String(s) => {
                    if *multiline {
                        s.to_str()?.to_string()
                    } else {
                        s.to_string_lossy()
                    }
                }
                other => {
                    return Err(runtime(format!(
                        "{what} must be a text, not {}.",
                        other.type_name()
                    )));
                }
            };
            if s.chars().count() > *max_len {
                return Err(runtime(format!("{what} is at most {max_len} characters.")));
            }
            ToolValue::Text(s)
        }
    })
}

/// A `{from, to}` pair (or `{ from =, to = }`).
fn pair_of_any(v: &Value, what: &str) -> mlua::Result<(i64, i64)> {
    let Value::Table(t) = v else {
        return Err(runtime(format!(
            "{what}: each entry is a pair {{from, to}}."
        )));
    };
    let a = match t.get::<Value>("from")? {
        Value::Nil => t.raw_get::<Value>(1)?,
        v => v,
    };
    let b = match t.get::<Value>("to")? {
        Value::Nil => t.raw_get::<Value>(2)?,
        v => v,
    };
    Ok((int_of(&a, what)?, int_of(&b, what)?))
}

/// A parameter declaration from its script table.
fn param_from_lua(t: &Table, index: usize) -> mlua::Result<ToolParam> {
    let what = format!("tool{{}} parameter {index}");
    let key: String = t
        .get::<Option<String>>("key")
        .map_err(|_| runtime(format!("{what}: key must be a text.")))?
        .ok_or_else(|| runtime(format!("{what}: key is missing.")))?;
    if key.is_empty()
        || key.len() > 32
        || !key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(runtime(format!(
            "{what}: the key \u{201C}{key}\u{201D} must be 1 – 32 characters of a – z, 0 – 9 or \u{201C}_\u{201D}."
        )));
    }
    let what = format!("Parameter \u{201C}{key}\u{201D}");
    let label = {
        let l = text_field(t, "label", &what, 60, false)?;
        if l.is_empty() {
            key.replace('_', " ")
        } else {
            l
        }
    };
    let help = text_field(t, "help", &what, 400, false)?;
    let kind_name: String = t
        .get::<Option<String>>("kind")
        .map_err(|_| runtime(format!("{what}: kind must be a text.")))?
        .ok_or_else(|| runtime(format!("{what}: kind is missing.")))?;
    let opt_int = |k: &str, d: i64| -> mlua::Result<i64> {
        match t.get::<Value>(k)? {
            Value::Nil => Ok(d),
            v => int_of(&v, &format!("{what}: {k}")),
        }
    };
    let opt_num = |k: &str, d: f64| -> mlua::Result<f64> {
        match t.get::<Value>(k)? {
            Value::Nil => Ok(d),
            v => num_of(&v, &format!("{what}: {k}")),
        }
    };
    let kind = match kind_name.as_str() {
        "int" => ToolParamKind::Int {
            min: opt_int("min", 0)?,
            max: opt_int("max", 100)?,
        },
        "float" => ToolParamKind::Float {
            min: opt_num("min", 0.0)?,
            max: opt_num("max", 1.0)?,
            unit: text_field(t, "unit", &what, 12, false)?,
        },
        "bool" => ToolParamKind::Bool,
        "choice" => {
            let list: Table = t
                .get::<Option<Table>>("choices")
                .map_err(|_| runtime(format!("{what}: choices must be a list of texts.")))?
                .ok_or_else(|| runtime(format!("{what}: choices are missing.")))?;
            let mut c = Vec::new();
            for s in list.sequence_values::<String>() {
                let s = s.map_err(|_| runtime(format!("{what}: choices must be texts.")))?;
                if s.is_empty() || s.chars().count() > 60 || c.contains(&s) {
                    return Err(runtime(format!(
                        "{what}: each choice is 1 – 60 characters and unique."
                    )));
                }
                c.push(s);
            }
            if c.is_empty() || c.len() > 64 {
                return Err(runtime(format!("{what}: give 1 – 64 choices.")));
            }
            ToolParamKind::Choice(c)
        }
        "pitch" => ToolParamKind::Pitch,
        "ticks" => {
            let min = opt_int("min", 1)?;
            let max = opt_int("max", 1 << 40)?;
            if min < 0 {
                return Err(runtime(format!("{what}: ticks are 0 or more.")));
            }
            ToolParamKind::Ticks {
                min: min as u64,
                max: max.max(min) as u64,
            }
        }
        "seed" => ToolParamKind::Seed,
        "color" => ToolParamKind::Color,
        "curve" => ToolParamKind::Curve,
        "range" => ToolParamKind::Range {
            min: opt_int("min", 0)?,
            max: opt_int("max", 127)?,
        },
        "subchannel_map" => ToolParamKind::SubchannelMap,
        "note_filter" => ToolParamKind::NoteFilter,
        "text" => {
            let n = opt_int("max_len", 200)?;
            let multiline = match t.get::<Value>("multiline")? {
                Value::Nil => false,
                Value::Boolean(v) => v,
                _ => return Err(runtime(format!("{what}: multiline must be a boolean."))),
            };
            ToolParamKind::Text {
                max_len: n.clamp(1, 4000) as usize,
                multiline,
            }
        }
        other => {
            return Err(runtime(format!(
                "{what}: unknown kind \u{201C}{other}\u{201D} (int, float, bool, choice, pitch, ticks, seed, color, curve, range, subchannel_map, note_filter or text)."
            )));
        }
    };
    match &kind {
        ToolParamKind::Int { min, max } | ToolParamKind::Range { min, max } if min > max => {
            return Err(runtime(format!("{what}: min is above max.")));
        }
        ToolParamKind::Float { min, max, .. } if min > max => {
            return Err(runtime(format!("{what}: min is above max.")));
        }
        _ => {}
    }
    let default = match t.get::<Value>("default")? {
        Value::Nil => match &kind {
            ToolParamKind::Int { min, max } => ToolValue::Int(0.clamp(*min, *max)),
            ToolParamKind::Float { min, max, .. } => ToolValue::Float(0.0f64.clamp(*min, *max)),
            ToolParamKind::Bool => ToolValue::Bool(false),
            ToolParamKind::Choice(_) => ToolValue::Choice(0),
            ToolParamKind::Pitch => ToolValue::Int(60),
            ToolParamKind::Ticks { min, max } => ToolValue::Int(480u64.clamp(*min, *max) as i64),
            ToolParamKind::Seed => ToolValue::Int(1),
            ToolParamKind::Color => ToolValue::Color([255, 255, 255]),
            ToolParamKind::Curve => ToolValue::Curve(Curve::new(vec![(0.0, 0.0), (1.0, 1.0)])),
            ToolParamKind::Range { min, max } => ToolValue::Range(*min, *max),
            ToolParamKind::SubchannelMap => ToolValue::Map(Vec::new()),
            ToolParamKind::NoteFilter => ToolValue::Filter(NoteFilter::default()),
            ToolParamKind::Text { .. } => ToolValue::Text(String::new()),
        },
        v => value_from_lua(&kind, &v, &format!("{what}'s default"))?,
    };
    Ok(ToolParam {
        key,
        label,
        help,
        kind,
        default,
    })
}

/// The `params` list of a declaration table (`what` names the declaring
/// call in errors).
pub(super) fn params_from_lua(t: &Table, what: &str) -> mlua::Result<Vec<ToolParam>> {
    let mut params: Vec<ToolParam> = Vec::new();
    if let Some(list) = t.get::<Option<Table>>("params")? {
        for (i, p) in list.sequence_values::<Table>().enumerate() {
            let p = p.map_err(|_| runtime(format!("{what}: params is a list of tables.")))?;
            let param = param_from_lua(&p, i + 1)?;
            if params.iter().any(|q| q.key == param.key) {
                return Err(runtime(format!(
                    "{what}: the parameter key \u{201C}{}\u{201D} is used twice.",
                    param.key
                )));
            }
            params.push(param);
            if params.len() > MAX_PARAMS {
                return Err(runtime(format!("{what}: at most {MAX_PARAMS} parameters.")));
            }
        }
    }
    Ok(params)
}

/// `ctx.params` from the raw values a run carries, through the schema
/// (missing values take their defaults; every value is checked again).
pub(super) fn params_to_ctx(lua: &Lua, params: &[ToolParam], raw: &Table) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for p in params {
        let v = raw.get::<Value>(p.key.as_str())?;
        let tv = if v.is_nil() {
            p.default.clone()
        } else {
            value_from_lua(&p.kind, &v, &format!("Parameter \u{201C}{}\u{201D}", p.key))?
        };
        out.set(p.key.as_str(), value_to_lua(lua, &p.kind, &tv)?)?;
    }
    Ok(out)
}

/// The tool declaration from `plum.plugin.tool{...}`'s table.
fn decl_from_lua(t: &Table) -> mlua::Result<(ToolDecl, Function)> {
    let id: String = t
        .get::<Option<String>>("id")
        .map_err(|_| runtime("tool{}: id must be a text."))?
        .ok_or_else(|| runtime("tool{}: id is missing."))?;
    check_decl_id(&id, "tool")?;
    let name = text_field(t, "name", "tool{}", 80, true)?;
    let tooltip = text_field(t, "tooltip", "tool{}", 400, false)?;
    let needs_selection = t.get::<Option<bool>>("needs_selection")?.unwrap_or(true);
    let freehand = match t.get::<Value>("gesture")? {
        Value::Nil => false,
        Value::String(v) if v.to_str()?.as_ref() == "freehand" => true,
        _ => {
            return Err(runtime(
                "tool{}: gesture must be the text freehand, or absent.",
            ));
        }
    };
    let run: Function = match t.get::<Value>("run")? {
        Value::Function(f) => f,
        _ => return Err(runtime("tool{}: run must be a function.")),
    };
    let params = params_from_lua(t, "tool{}")?;
    Ok((
        ToolDecl {
            id,
            name,
            tooltip,
            params,
            needs_selection,
            freehand,
        },
        run,
    ))
}

/// A value as a script sees it in `ctx.params`.
fn value_to_lua(lua: &Lua, kind: &ToolParamKind, v: &ToolValue) -> mlua::Result<Value> {
    Ok(match v {
        ToolValue::Int(n) => Value::Integer(*n),
        ToolValue::Float(f) => Value::Number(*f),
        ToolValue::Bool(b) => Value::Boolean(*b),
        ToolValue::Choice(i) => match kind {
            ToolParamKind::Choice(c) => {
                Value::String(lua.create_string(c.get(*i).map_or("", String::as_str))?)
            }
            _ => Value::Integer(*i as i64),
        },
        ToolValue::Text(s) => Value::String(lua.create_string(s)?),
        ToolValue::Color([r, g, b]) => {
            let t = lua.create_table()?;
            t.set("r", i64::from(*r))?;
            t.set("g", i64::from(*g))?;
            t.set("b", i64::from(*b))?;
            Value::Table(t)
        }
        ToolValue::Curve(c) => {
            // { points = {{x, y}, …}, at = function(x) } — `at` is the same
            // evaluation the built-in tools use.
            let t = lua.create_table()?;
            let pts = lua.create_table()?;
            for (i, (x, y)) in c.points.iter().enumerate() {
                let p = lua.create_sequence_from([*x, *y])?;
                pts.raw_set(i + 1, p)?;
            }
            t.set("points", pts)?;
            let curve = c.clone();
            t.set(
                "at",
                lua.create_function(move |_, x: f64| Ok(curve.eval(x)))?,
            )?;
            Value::Table(t)
        }
        ToolValue::Range(lo, hi) => {
            let t = lua.create_sequence_from([*lo, *hi])?;
            t.set("lo", *lo)?;
            t.set("hi", *hi)?;
            Value::Table(t)
        }
        ToolValue::Map(m) => {
            let t = lua.create_table()?;
            for (i, (a, b)) in m.iter().enumerate() {
                let e = lua.create_table()?;
                e.set("from", i64::from(*a))?;
                e.set("to", i64::from(*b))?;
                t.raw_set(i + 1, e)?;
            }
            Value::Table(t)
        }
        ToolValue::Filter(f) => {
            // The shape `track:notes{}` takes.
            let t = lua.create_table()?;
            t.set(
                "keys",
                lua.create_sequence_from([i64::from(f.keys.0), i64::from(f.keys.1)])?,
            )?;
            t.set(
                "velocity",
                lua.create_sequence_from([i64::from(f.velocity.0), i64::from(f.velocity.1)])?,
            )?;
            if let Some(c) = f.channel {
                t.set("channel", i64::from(c))?;
            }
            Value::Table(t)
        }
    })
}

/// The parameters as a Lua table literal for a run (`ctx.params`): plain
/// data only (numbers, booleans, quoted texts, nested tables), so a job's
/// source can carry them safely.
pub fn params_literal(decl: &ToolDecl, values: &[ToolValue]) -> String {
    values_literal(&decl.params, values)
}

/// [`params_literal`] for any parameter list (a command's options).
pub fn values_literal(params: &[ToolParam], values: &[ToolValue]) -> String {
    fn q(s: &str) -> String {
        let mut o = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                c if (c as u32) < 0x20 || c == '\u{7f}' => {
                    o.push_str(&format!("\\{:03}", c as u32))
                }
                c => o.push(c),
            }
        }
        o.push('"');
        o
    }
    fn num(f: f64) -> String {
        if f.is_finite() {
            format!("{f:?}")
        } else {
            "0".into()
        }
    }
    let mut out = String::from("{");
    for (p, v) in params.iter().zip(values) {
        let val = match v {
            ToolValue::Int(n) => n.to_string(),
            ToolValue::Float(f) => num(*f),
            ToolValue::Bool(b) => b.to_string(),
            ToolValue::Choice(i) => match &p.kind {
                ToolParamKind::Choice(c) => q(c.get(*i).map_or("", String::as_str)),
                _ => i.to_string(),
            },
            ToolValue::Text(s) => q(s),
            ToolValue::Color([r, g, b]) => format!("{{r={r},g={g},b={b}}}"),
            ToolValue::Curve(c) => {
                let pts: Vec<String> = c
                    .points
                    .iter()
                    .map(|(x, y)| format!("{{{},{}}}", num(*x), num(*y)))
                    .collect();
                format!("{{{}}}", pts.join(","))
            }
            ToolValue::Range(lo, hi) => format!("{{{lo},{hi}}}"),
            ToolValue::Map(m) => {
                let e: Vec<String> = m.iter().map(|(a, b)| format!("{{{a},{b}}}")).collect();
                format!("{{{}}}", e.join(","))
            }
            ToolValue::Filter(f) => format!(
                "{{keys={{{},{}}},velocity={{{},{}}}{}}}",
                f.keys.0,
                f.keys.1,
                f.velocity.0,
                f.velocity.1,
                f.channel.map_or(String::new(), |c| format!(",channel={c}"))
            ),
        };
        out.push_str(&format!("{}={},", p.key, val));
    }
    out.push('}');
    out
}

/// Registers `plum.plugin.tool` and the host's `__run_tool` in `p`.
pub(super) fn install(lua: &Lua, p: &Table) -> mlua::Result<()> {
    p.set(
        "tool",
        lua.create_function(|lua, t: Table| {
            require_loading(lua, "A tool")?;
            let (decl, run) = decl_from_lua(&t)?;
            let mut slot = lua
                .app_data_mut::<PluginSlot>()
                .ok_or_else(|| runtime("Only plugins can register tools."))?;
            if slot.decls.tools.iter().any(|d| d.id == decl.id) {
                return Err(runtime(format!(
                    "The tool \u{201C}{}\u{201D} is already registered by this plugin.",
                    decl.id
                )));
            }
            let id = decl.id.clone();
            slot.decls.tools.push(decl);
            slot.changed = true;
            drop(slot);
            let fns: Table = lua.named_registry_value(TOOLS)?;
            fns.set(id, run)?;
            Ok(())
        })?,
    )?;
    p.set(
        "__run_tool",
        lua.create_function(
            |lua, (id, raw, info): (String, Table, Table)| -> mlua::Result<Value> {
                let decl = lua
                    .app_data_ref::<PluginSlot>()
                    .and_then(|s| s.decls.tools.iter().find(|d| d.id == id).cloned())
                    .ok_or_else(|| {
                        runtime(format!(
                            "The tool \u{201C}{id}\u{201D} is not registered by this plugin."
                        ))
                    })?;
                let fns: Table = lua.named_registry_value(TOOLS)?;
                let run: Function = match fns.get::<Value>(id.as_str())? {
                    Value::Function(f) => f,
                    _ => {
                        return Err(runtime(format!(
                            "The tool \u{201C}{id}\u{201D} has no run function."
                        )));
                    }
                };
                // The values again through the schema (what the script sees is
                // always valid), curves as objects.
                let params = params_to_ctx(lua, &decl.params, &raw)?;
                let w = current_world(lua).ok_or_else(|| runtime("No project is open."))?;
                let track = info.get::<Option<u32>>("track")?.map(TrackId);
                let content = info.get::<Option<u32>>("content")?.map(ContentId);
                let target = match (content, track) {
                    (Some(c), Some(t)) => w.content_on_track(c, t),
                    (Some(c), None) => w.content(c),
                    (None, Some(t)) => w.track(t),
                    _ => None,
                }
                .ok_or_else(|| runtime("The track or clip this tool edits no longer exists."))?;
                let target = lua.create_userdata(LuaHandle(target))?;
                // The notes it works on: the selection, or (tools that allow it)
                // every note when nothing is selected.
                let sel_q = lua.create_table()?;
                sel_q.set("selected", true)?;
                let selected: mlua::AnyUserData = target.call_method("notes", sel_q)?;
                let n_sel: i64 = selected.call_method("count", ())?;
                let notes = if n_sel > 0 {
                    selected
                } else if decl.needs_selection {
                    return Err(runtime(format!(
                        "{} works on selected notes: select some first.",
                        decl.name
                    )));
                } else {
                    target.call_method::<mlua::AnyUserData>("notes", ())?
                };
                let ctx = lua.create_table()?;
                ctx.set("tool", id.as_str())?;
                ctx.set("params", params)?;
                ctx.set("track", target)?;
                // Content handles retain API01 identity. Expose the captured editing
                // owner explicitly; never infer a linked clip's owner by placement order.
                ctx.set(
                    "owner_track",
                    super::handles::handle_or_nil(lua, track.and_then(|t| w.track(t)))?,
                )?;
                ctx.set("notes", notes)?;
                ctx.set("selection", n_sel > 0)?;
                ctx.set(
                    "preview",
                    info.get::<Option<bool>>("preview")?.unwrap_or(false),
                )?;
                ctx.set("seed", info.get::<Option<i64>>("seed")?.unwrap_or(1))?;
                let keys = info.get::<Option<u16>>("keys")?.unwrap_or(128);
                ctx.set("keys", keys)?;
                if decl.freehand {
                    ctx.set(
                        "stroke",
                        super::drawing::for_tool(
                            lua,
                            track
                                .ok_or_else(|| runtime("Drawing needs an editing track."))?
                                .0,
                            content.map(|c| c.0),
                            keys,
                        )?,
                    )?;
                }
                // One transaction labelled with the tool's name = one Undo step.
                let plum: Table = lua.globals().get("plum")?;
                let open: Function = plum.get("transaction")?;
                let label = lua.create_table()?;
                label.set("label", decl.name.as_str())?;
                let tx: mlua::AnyUserData = open.call(label)?;
                ctx.set("tx", tx.clone())?;
                let r = run.call::<Value>(ctx)?;
                if tx.call_method::<bool>("is_open", ())? {
                    tx.call_method::<()>("commit", ())?;
                }
                Ok(r)
            },
        )?,
    )?;
    Ok(())
}
