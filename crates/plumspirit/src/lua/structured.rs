//! Bounded plain data crossing host-owned worker boundaries. No Lua references,
//! metatables, executable values or opaque handles are transferred.
use super::bridge::{NativeBudget, Reservation};
use super::{Category, stop};
use mlua::{Lua, Value};
use serde_json::{Map, Value as Json};
use std::sync::Arc;

pub const MAX_BYTES: usize = 64 * 1024;
pub const MAX_NODES: usize = 2048;
pub const MAX_DEPTH: usize = 8;
pub const MAX_STRING: usize = 16 * 1024;
pub const MAX_KEY: usize = 128;
const NATIVE_BYTES: usize = 512 * 1024;
#[derive(Clone)]
pub struct Payload(Arc<Inner>);
struct Inner {
    value: Json,
    _native: Reservation,
}
impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Payload").field(&self.0.value).finish()
    }
}
impl PartialEq for Payload {
    fn eq(&self, other: &Self) -> bool {
        self.0.value == other.0.value
    }
}
impl Payload {
    pub fn read(lua: &Lua, value: Value) -> mlua::Result<Self> {
        let budget = lua
            .app_data_ref::<NativeBudget>()
            .ok_or_else(|| invalid("Native data quota is unavailable."))?
            .clone();
        let native = budget.reserve(NATIVE_BYTES).map_err(|e| e.lua())?;
        let mut reader = Reader {
            lua,
            nodes: 0,
            bytes: 0,
            path: [0; MAX_DEPTH + 1],
        };
        let value = reader.value(value, 0)?;
        Ok(Self(Arc::new(Inner {
            value,
            _native: native,
        })))
    }
    pub fn to_lua(&self, lua: &Lua) -> mlua::Result<Value> {
        super::notes::check_budget(lua)?;
        super::config::from_json(lua, &self.0.value)
    }
    pub(crate) fn json(&self) -> &Json {
        &self.0.value
    }
    #[cfg(test)]
    fn value(&self) -> &Json {
        &self.0.value
    }
}
fn invalid(message: impl Into<String>) -> mlua::Error {
    stop(Category::Runtime, message)
}
struct Reader<'a> {
    lua: &'a Lua,
    nodes: usize,
    bytes: usize,
    path: [usize; MAX_DEPTH + 1],
}
impl Reader<'_> {
    fn charge(&mut self, bytes: usize) -> mlua::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| {
                stop(
                    Category::Budget,
                    "Structured data exceeds 64 KiB encoded size.",
                )
            })?;
        Ok(())
    }
    fn text(&mut self, value: mlua::LuaString, key: bool) -> mlua::Result<String> {
        let bytes = value.as_bytes();
        if bytes.len() > if key { MAX_KEY } else { MAX_STRING } {
            return Err(stop(
                Category::Budget,
                "Structured text or key exceeds its byte limit.",
            ));
        }
        let value = value
            .to_str()
            .map_err(|_| invalid("Structured text must be UTF-8."))?;
        let escaped = value
            .as_bytes()
            .iter()
            .map(|b| match b {
                b'"' | b'\\' => 2,
                0..=31 => 6,
                _ => 1,
            })
            .sum::<usize>();
        self.charge(escaped + 2)?;
        Ok(value.to_owned())
    }
    fn value(&mut self, value: Value, depth: usize) -> mlua::Result<Json> {
        if depth > MAX_DEPTH {
            return Err(stop(
                Category::Budget,
                "Structured data nests deeper than 8 levels.",
            ));
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(stop(
                Category::Budget,
                "Structured data exceeds 2,048 values.",
            ));
        }
        if self.nodes.is_multiple_of(64) {
            super::notes::check_budget(self.lua)?;
        }
        Ok(match value {
            Value::Nil => {
                self.charge(4)?;
                Json::Null
            }
            Value::Boolean(b) => {
                self.charge(5)?;
                Json::Bool(b)
            }
            Value::Integer(i) => {
                self.charge(20)?;
                Json::from(i)
            }
            Value::Number(n) => {
                let number = serde_json::Number::from_f64(n)
                    .ok_or_else(|| invalid("Structured numbers must be finite."))?;
                self.charge(32)?;
                Json::Number(number)
            }
            Value::String(s) => Json::String(self.text(s, false)?),
            Value::Table(t) => {
                if t.metatable().is_some() {
                    return Err(invalid("Structured tables must have no metatable."));
                }
                let pointer = t.to_pointer() as usize;
                if self.path[..depth].contains(&pointer) {
                    return Err(invalid("Structured tables must not contain cycles."));
                }
                self.path[depth] = pointer;
                let len = t.raw_len();
                if len > MAX_NODES.saturating_sub(self.nodes) {
                    return Err(stop(
                        Category::Budget,
                        "Structured array exceeds its value budget.",
                    ));
                }
                self.charge(2)?;
                if len > 0 {
                    let mut out = Vec::with_capacity(len);
                    // Check key shape before recursive allocation, without __pairs/__index.
                    let mut count = 0;
                    for pair in t.clone().pairs::<Value, Value>() {
                        let (key, _) = pair?;
                        count += 1;
                        if !matches!(key,Value::Integer(i) if i>0 && (i as u64)<=len as u64)
                            || count > len
                        {
                            return Err(invalid(
                                "Structured arrays must be dense and contain only their numbered keys.",
                            ));
                        }
                    }
                    if count != len {
                        return Err(invalid("Structured arrays must be dense."));
                    }
                    for i in 1..=len {
                        self.charge(1)?;
                        out.push(self.value(t.raw_get(i)?, depth + 1)?);
                    }
                    Json::Array(out)
                } else {
                    let mut out = Map::new();
                    for pair in t.pairs::<Value, Value>() {
                        let (key, value) = pair?;
                        if self.nodes >= MAX_NODES {
                            return Err(stop(
                                Category::Budget,
                                "Structured object exceeds its value budget.",
                            ));
                        }
                        let Value::String(key) = key else {
                            return Err(invalid("Structured object keys must be text."));
                        };
                        let key = self.text(key, true)?;
                        self.charge(2)?;
                        out.insert(key, self.value(value, depth + 1)?);
                    }
                    Json::Object(out)
                }
            }
            _ => {
                return Err(invalid(
                    "Structured data accepts only nil, booleans, finite numbers, UTF-8 text and plain tables.",
                ));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::{Host, Limits};
    #[test]
    fn structured_data_preserves_exact_types_and_shares_native_ownership() {
        let host = Host::new(Limits::default(), []).unwrap();
        let budget = NativeBudget::isolated(NATIVE_BYTES * 2);
        host.lua.set_app_data(budget.clone());
        let value = host
            .lua
            .load("return {tick=9007199254740993, text='a\0b', values={true,2.5,'x'}, empty={}}")
            .eval::<Value>()
            .unwrap();
        let payload = Payload::read(&host.lua, value).unwrap();
        assert_eq!(
            payload.value()["tick"].as_i64(),
            Some(9_007_199_254_740_993)
        );
        assert_eq!(payload.value()["text"].as_str(), Some("a\0b"));
        let shared = payload.clone();
        drop(payload);
        assert_eq!(budget.used(), NATIVE_BYTES);
        host.lua
            .globals()
            .set("roundtrip", shared.to_lua(&host.lua).unwrap())
            .unwrap();
        host.lua.load("assert(math.type(roundtrip.tick)=='integer' and roundtrip.tick==9007199254740993);assert(roundtrip.values[2]==2.5)").exec().unwrap();
        drop(shared);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn structured_data_rejects_executable_sparse_cyclic_deep_and_large_values_before_transfer() {
        let host = Host::new(Limits::default(), []).unwrap();
        let budget = NativeBudget::isolated(NATIVE_BYTES);
        host.lua.set_app_data(budget.clone());
        let cases = [
            "return function()end",
            "return 0/0",
            "return math.huge",
            "return setmetatable({}, {})",
            "return {[1]=1,[3]=3}",
            "return {[1]=1,name='mixed'}",
            "return {[false]=1}",
            "local t={};t.self=t;return t",
            "local t={};local r=t;for i=1,10 do t.child={};t=t.child end;return r",
            "return string.rep('x',16385)",
            "return {[string.rep('k',129)]=1}",
            "local t={};for i=1,2049 do t[i]=i end;return t",
            "return {string.rep('x',16384),string.rep('x',16384),string.rep('x',16384),string.rep('x',16384)}",
            "return string.char(255)",
        ];
        for source in cases {
            let value = host.lua.load(source).eval::<Value>().unwrap();
            assert!(
                Payload::read(&host.lua, value).is_err(),
                "accepted {source}"
            );
            assert_eq!(budget.used(), 0);
        }
        host.lua.set_app_data(NativeBudget::isolated(0));
        assert!(Payload::read(&host.lua, Value::Nil).is_err());
    }
}
