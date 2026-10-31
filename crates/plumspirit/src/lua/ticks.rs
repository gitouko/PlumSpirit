//! Exact 64-bit ticks in Lua (v1.0-beta1 B1-0023, `docs/LUA_API.md`
//! "Numbers").
//!
//! PlumSpirit's ticks are `u64`; Lua 5.4 integers are `i64`. Every tick the
//! API hands out is a Lua **integer** (never a float), so the reachable range
//! is 0 – 2⁶³ − 1; a document value beyond it (unreachable in practice) is an
//! error, never a wrapped or rounded number. Ticks a script passes in may be
//! integers, floats that are whole numbers no larger than 2⁵³ (above that a
//! float has already lost precision), or decimal strings. Lua's own integer
//! arithmetic wraps around silently on overflow, so `plum.ticks` offers
//! checked arithmetic that fails instead.

use mlua::{Lua, Table, Value};

use super::{Category, stop};

/// The largest tick a Lua integer holds (2⁶³ − 1).
pub const MAX_LUA_TICK: u64 = i64::MAX as u64;

/// The largest whole number every float represents exactly (2⁵³).
pub const MAX_EXACT_FLOAT: f64 = 9_007_199_254_740_992.0;

fn bad(msg: String) -> mlua::Error {
    stop(Category::Runtime, msg)
}

/// A document tick as a Lua integer.
pub fn to_lua(t: u64) -> mlua::Result<i64> {
    i64::try_from(t).map_err(|_| {
        bad(format!(
            "The tick {t} is past 2^63 - 1 ({MAX_LUA_TICK}), the largest Lua integer, and cannot be given to a script exactly."
        ))
    })
}

/// A tick from a script: a non-negative integer, a whole float ≤ 2⁵³, or a
/// decimal string (`what` names the argument in errors).
pub fn from_lua(v: &Value, what: &str) -> mlua::Result<u64> {
    match v {
        Value::Integer(i) => {
            u64::try_from(*i).map_err(|_| bad(format!("{what} must not be negative (it is {i}).")))
        }
        Value::Number(n) => {
            let n = *n;
            if !n.is_finite() || n.fract() != 0.0 {
                Err(bad(format!(
                    "{what} must be a whole number of ticks (it is {n})."
                )))
            } else if n < 0.0 {
                Err(bad(format!("{what} must not be negative (it is {n}).")))
            } else if n > MAX_EXACT_FLOAT {
                Err(bad(format!(
                    "{what} is a float above 2^53 ({n:.0}), which may already have lost precision; pass an integer or a decimal string."
                )))
            } else {
                Ok(n as u64)
            }
        }
        Value::String(s) => parse_decimal(&s.to_string_lossy(), what),
        other => Err(bad(format!(
            "{what} must be a tick (an integer, a whole number or a decimal string), not {}.",
            other.type_name()
        ))),
    }
}

/// A decimal tick string (optional `+`, digits, `_` separators), checked to
/// 2⁶³ − 1.
pub fn parse_decimal(s: &str, what: &str) -> mlua::Result<u64> {
    let t = s.trim();
    let digits: String = t
        .strip_prefix('+')
        .unwrap_or(t)
        .chars()
        .filter(|&c| c != '_')
        .collect();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad(format!(
            "{what} \u{201C}{s}\u{201D} is not a decimal number of ticks."
        )));
    }
    match digits.parse::<u64>() {
        Ok(v) if v <= MAX_LUA_TICK => Ok(v),
        _ => Err(bad(format!(
            "{what} \u{201C}{s}\u{201D} is past 2^63 - 1 ({MAX_LUA_TICK}), the largest tick a script can hold."
        ))),
    }
}

fn checked(r: Option<u64>, what: impl FnOnce() -> String) -> mlua::Result<i64> {
    match r {
        Some(v) if v <= MAX_LUA_TICK => Ok(v as i64),
        _ => Err(bad(what())),
    }
}

/// Installs `plum.ticks`.
pub fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set("max", i64::MAX)?;
    t.set("exact_float_max", 9_007_199_254_740_992i64)?;
    t.set(
        "check",
        lua.create_function(|_, v: Value| to_lua(from_lua(&v, "The tick")?))?,
    )?;
    t.set(
        "parse",
        lua.create_function(|_, s: String| to_lua(parse_decimal(&s, "The tick")?))?,
    )?;
    t.set(
        "add",
        lua.create_function(|_, (a, b): (Value, Value)| {
            let (a, b) = (from_lua(&a, "a")?, from_lua(&b, "b")?);
            checked(a.checked_add(b), || {
                format!("{a} + {b} is past 2^63 - 1, the largest tick.")
            })
        })?,
    )?;
    t.set(
        "sub",
        lua.create_function(|_, (a, b): (Value, Value)| {
            let (a, b) = (from_lua(&a, "a")?, from_lua(&b, "b")?);
            checked(a.checked_sub(b), || format!("{a} - {b} is before tick 0."))
        })?,
    )?;
    t.set(
        "mul",
        lua.create_function(|_, (a, b): (Value, Value)| {
            let (a, b) = (from_lua(&a, "a")?, from_lua(&b, "b")?);
            checked(a.checked_mul(b), || {
                format!("{a} * {b} is past 2^63 - 1, the largest tick.")
            })
        })?,
    )?;
    t.set(
        "muldiv",
        lua.create_function(
            |_, (a, num, den, mode): (Value, Value, Value, Option<String>)| {
                let a = from_lua(&a, "a")?;
                let num = from_lua(&num, "the numerator")?;
                let den = from_lua(&den, "the denominator")?;
                if den == 0 {
                    return Err(bad("The denominator must not be 0.".into()));
                }
                let p = u128::from(a) * u128::from(num);
                let d = u128::from(den);
                let q = match mode.as_deref().unwrap_or("floor") {
                    "floor" => p / d,
                    "ceil" => p.div_ceil(d),
                    // Halves away from zero (ticks are never negative).
                    "round" => (p + d / 2) / d,
                    other => {
                        return Err(bad(format!(
                            "The rounding \u{201C}{other}\u{201D} is not \"floor\", \"ceil\" or \"round\"."
                        )));
                    }
                };
                checked(u64::try_from(q).ok(), || {
                    format!("{a} * {num} / {den} is past 2^63 - 1, the largest tick.")
                })
            },
        )?,
    )?;
    plum.set("ticks", t)?;
    Ok(())
}
