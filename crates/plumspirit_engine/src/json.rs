//! A small JSON reader / writer (std only) for the formats the engine shares
//! with other programs: Spiderweb shapes (`spiderweb`), share codes. Objects
//! keep their key order. Numbers are `f64` (JSON has no integer type; every
//! integer up to 2^53 is exact).

use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

/// Deepest nesting accepted (malicious inputs cannot overflow the stack).
const MAX_DEPTH: usize = 256;

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Bool(b) => Some(f64::from(u8::from(*b))),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Python truthiness (`if x:`).
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Arr(v) => !v.is_empty(),
            Value::Obj(v) => !v.is_empty(),
        }
    }

    /// Sets `key` in an object (replacing or appending).
    pub fn set(&mut self, key: &str, v: Value) {
        if let Value::Obj(o) = self {
            if let Some(e) = o.iter_mut().find(|(k, _)| k == key) {
                e.1 = v;
            } else {
                o.push((key.to_owned(), v));
            }
        }
    }

    pub fn obj() -> Value {
        Value::Obj(Vec::new())
    }

    /// Compact text like Python's `json.dumps(x, separators=(", ", ": "))`
    /// (Python's default separators).
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    fn write(&self, s: &mut String) {
        match self {
            Value::Null => s.push_str("null"),
            Value::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
            Value::Num(n) => write_num(s, *n),
            Value::Str(t) => write_str(s, t),
            Value::Arr(v) => {
                s.push('[');
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    x.write(s);
                }
                s.push(']');
            }
            Value::Obj(v) => {
                s.push('{');
                for (i, (k, x)) in v.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    write_str(s, k);
                    s.push_str(": ");
                    x.write(s);
                }
                s.push('}');
            }
        }
    }
}

fn write_num(s: &mut String, n: f64) {
    if !n.is_finite() {
        // Python writes NaN / Infinity; JSON readers elsewhere refuse them.
        s.push_str(if n.is_nan() {
            "NaN"
        } else if n > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        });
    } else if n == n.trunc() && n.abs() < 1e16 {
        if n == 0.0 && n.is_sign_negative() {
            s.push_str("-0");
        } else {
            let _ = write!(s, "{}", n as i64);
        }
    } else {
        // Shortest round-trip representation.
        let _ = write!(s, "{n:?}");
    }
}

fn write_str(s: &mut String, t: &str) {
    s.push('"');
    for c in t.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7E => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    let _ = write!(s, "\\u{u:04x}");
                }
            }
            c => s.push(c),
        }
    }
    s.push('"');
}

/// Parses JSON text (also Python's NaN / Infinity). Errors are sentences.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("Unexpected text after the JSON value at {}.", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("Invalid JSON at {}: {what}.", self.i))
    }

    fn lit(&mut self, s: &str) -> bool {
        if self.b[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return self.err("nested too deeply");
        }
        let Some(&c) = self.b.get(self.i) else {
            return self.err("unexpected end");
        };
        match c {
            b'{' => {
                self.i += 1;
                let mut o = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(o));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return self.err("expected a key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return self.err("expected ':'");
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    if let Some(e) = o.iter_mut().find(|(x, _): &&mut (String, Value)| *x == k) {
                        e.1 = v; // the last duplicate wins (Python)
                    } else {
                        o.push((k, v));
                    }
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(o));
                        }
                        _ => return self.err("expected ',' or '}'"),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Value::Arr(v));
                }
                loop {
                    self.ws();
                    v.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(v));
                        }
                        _ => return self.err("expected ',' or ']'"),
                    }
                }
            }
            b'"' => Ok(Value::Str(self.string()?)),
            b't' if self.lit("true") => Ok(Value::Bool(true)),
            b'f' if self.lit("false") => Ok(Value::Bool(false)),
            b'n' if self.lit("null") => Ok(Value::Null),
            b'N' if self.lit("NaN") => Ok(Value::Num(f64::NAN)),
            b'I' if self.lit("Infinity") => Ok(Value::Num(f64::INFINITY)),
            b'-' if self.lit("-Infinity") => Ok(Value::Num(f64::NEG_INFINITY)),
            b'-' | b'0'..=b'9' => {
                let st = self.i;
                if self.b[self.i] == b'-' {
                    self.i += 1;
                }
                while self.i < self.b.len()
                    && matches!(
                        self.b[self.i],
                        b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                    )
                {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.b[st..self.i]).unwrap_or("");
                match t.parse::<f64>() {
                    Ok(n) => Ok(Value::Num(n)),
                    Err(_) => self.err("bad number"),
                }
            }
            _ => self.err("unexpected character"),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let Some(h) = self.b.get(self.i..self.i + 4) else {
            return self.err("short \\u escape");
        };
        let s = std::str::from_utf8(h).map_err(|_| "Invalid JSON escape.".to_string())?;
        let v = u32::from_str_radix(s, 16).map_err(|_| "Invalid JSON escape.".to_string())?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let Some(&c) = self.b.get(self.i) else {
                return self.err("unterminated string");
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let Some(&e) = self.b.get(self.i) else {
                        return self.err("unterminated escape");
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xD800..0xDC00).contains(&u) && self.lit("\\u") {
                                let lo = self.hex4()?;
                                u = 0x10000
                                    + ((u - 0xD800) << 10)
                                    + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            out.push(char::from_u32(u).unwrap_or('\u{FFFD}'));
                        }
                        _ => return self.err("bad escape"),
                    }
                }
                _ => {
                    // A whole UTF-8 sequence.
                    let st = self.i;
                    self.i += 1;
                    while self.i < self.b.len() && (self.b[self.i] & 0xC0) == 0x80 {
                        self.i += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.b[st..self.i]).unwrap_or("\u{FFFD}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_zero_preserves_the_surrounding_document_and_its_sign() {
        let text = "{\"points\": [[0.123456789012345, -0], [1e-9, -2.5000001]]}";
        let value = parse(text).unwrap();
        let encoded = value.to_text();
        assert!(encoded.starts_with("{\"points\":"), "{encoded}");
        let reopened = parse(&encoded).unwrap();
        assert_eq!(reopened, value);
        let zero = reopened.get("points").unwrap().as_arr().unwrap()[0]
            .as_arr()
            .unwrap()[1]
            .as_f64()
            .unwrap();
        assert_eq!(zero.to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn json_round_trips_and_refuses_garbage() {
        let t = r#"{"kind": "line", "pts": [[0, 60.5], [2.25, -1e-3]], "on": true, "n": null, "s": "a\"é\n"}"#;
        let v = parse(t).unwrap();
        assert_eq!(v.get("kind").and_then(Value::as_str), Some("line"));
        let pts = v.get("pts").unwrap().as_arr().unwrap();
        assert_eq!(pts[1].as_arr().unwrap()[1].as_f64(), Some(-0.001));
        assert_eq!(parse(&v.to_text()).unwrap(), v);
        assert!(
            v.to_text()
                .starts_with(r#"{"kind": "line", "pts": [[0, 60.5]"#)
        );
        for bad in ["", "{", "[1,]", "{\"a\" 1}", "tru", "\"x", "[1] 2"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let deep = "[".repeat(1000) + &"]".repeat(1000);
        assert!(parse(&deep).is_err());
    }
}
