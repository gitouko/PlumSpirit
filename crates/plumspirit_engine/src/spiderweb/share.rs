//! Shape share codes (Spiderweb's files/share.py): copied shapes or a
//! drawing as one line of text, `"SPIDERWEB1:" + base64(zlib(JSON))`, that
//! pastes back into Spiderweb (or PlumSpirit) anywhere. Everything read goes
//! through the same checks as a project file.

use super::areas::clean_areas;
use super::custom::clean_strokes;
use crate::json::{self, Value};

pub const VERSION: u32 = 1;

/// Why a line couldn't be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareError {
    /// no shared line in the text
    None,
    /// made by a newer Spiderweb
    Newer,
    /// cut off or changed
    Broken,
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ShareError::None => "There is no Spiderweb share code in the text.",
            ShareError::Newer => "The share code was made by a newer Spiderweb.",
            ShareError::Broken => "The share code is cut off or was changed.",
        })
    }
}

/// `base64.b64decode(text, validate=True)` (binascii strict mode).
fn b64decode_strict(text: &str) -> Option<Vec<u8>> {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let data = text.as_bytes();
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let (mut quad, mut left, mut pads, mut padding) = (0u8, 0u32, 0u32, false);
    for (i, &c) in data.iter().enumerate() {
        if c == b'=' {
            padding = true;
            if quad == 0 {
                return None;
            }
            if quad >= 2 {
                pads += 1;
                if u32::from(quad) + pads >= 4 {
                    return (i + 1 >= data.len()).then_some(out);
                }
            }
            continue;
        }
        let v = B64.iter().position(|&b| b == c)? as u32;
        if padding {
            return None;
        }
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = v;
            }
            1 => {
                quad = 2;
                out.push(((left << 2) | (v >> 4)) as u8);
                left = v & 0x0f;
            }
            2 => {
                quad = 3;
                out.push((((left << 4) | (v >> 2)) & 0xff) as u8);
                left = v & 0x03;
            }
            _ => {
                quad = 0;
                out.push((((left << 6) | v) & 0xff) as u8);
                left = 0;
            }
        }
    }
    (quad == 0).then_some(out)
}

/// The first `SPIDERWEB<digits>:<base64>` in the text: (version, body).
fn find(text: &str) -> Option<(&str, &str)> {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(i) = text[from..].find("SPIDERWEB") {
        let s = from + i + 9;
        let mut j = s;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > s && j < b.len() && b[j] == b':' {
            let mut k = j + 1;
            while k < b.len()
                && (b[k].is_ascii_alphanumeric() || matches!(b[k], b'+' | b'/' | b'='))
            {
                k += 1;
            }
            return Some((&text[s..j], &text[j + 1..k]));
        }
        from = from + i + 1;
    }
    None
}

/// A JSON text → the shared line.
pub fn pack(body: &str) -> String {
    let z = crate::codec::zlib_compress(body.as_bytes());
    format!("SPIDERWEB{VERSION}:{}", crate::codec::b64encode(&z))
}

/// A text holding a shared line → the unpacked object.
pub fn unpack(text: &str) -> Result<Value, ShareError> {
    let (ver, body) = find(text).ok_or(ShareError::None)?;
    let newer = ver.trim_start_matches('0').len() > 9
        || ver.parse::<u64>().map_or(true, |v| v > u64::from(VERSION));
    if newer {
        return Err(ShareError::Newer);
    }
    let raw = b64decode_strict(body).ok_or(ShareError::Broken)?;
    let data = crate::codec::zlib_decompress(&raw, 256 << 20).ok_or(ShareError::Broken)?;
    let s = String::from_utf8(data).map_err(|_| ShareError::Broken)?;
    let got = json::parse(&s).map_err(|_| ShareError::Broken)?;
    if !matches!(got, Value::Obj(_))
        || !matches!(super::gets(&got, "kind"), Some("shapes" | "drawing"))
    {
        return Err(ShareError::Broken);
    }
    Ok(got)
}

/// An unpacked "shapes" object → the shapes, checked like a project's.
pub fn read_shapes(got: &Value) -> Result<Vec<Value>, ShareError> {
    let out: Vec<Value> = got
        .get("shapes")
        .and_then(Value::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter(|s| matches!(s, Value::Obj(_)))
        .filter_map(super::clean_shape)
        .collect();
    if out.is_empty() {
        return Err(ShareError::Broken);
    }
    Ok(out)
}

/// A drawing: name, strokes, areas.
pub type Drawing = (String, Vec<Value>, Vec<(f64, f64, i64)>);

/// An unpacked "drawing" → (name, strokes, areas).
pub fn read_drawing(got: &Value) -> Result<Drawing, ShareError> {
    let strokes = clean_strokes(got.get("strokes"));
    if strokes.is_empty() {
        return Err(ShareError::Broken);
    }
    let name = got
        .get("name")
        .filter(|n| n.truthy())
        .map_or_else(String::new, super::pattern::py_str);
    Ok((name, strokes, clean_areas(got.get("areas"))))
}

/// Shapes (as saved) → the shared line.
pub fn shapes_line(shapes: &[Value]) -> String {
    let mut body = Value::obj();
    body.set("kind", Value::Str("shapes".into()));
    body.set("shapes", Value::Arr(shapes.to_vec()));
    pack(&body.to_text())
}

/// A drawing → the shared line.
pub fn drawing_line(name: &str, strokes: &[Value], areas: &[(f64, f64, i64)]) -> String {
    let mut body = Value::obj();
    body.set("kind", Value::Str("drawing".into()));
    body.set("name", Value::Str(name.to_owned()));
    body.set("strokes", Value::Arr(strokes.to_vec()));
    if !areas.is_empty() {
        body.set(
            "areas",
            Value::Arr(
                areas
                    .iter()
                    .map(|(u, v, c)| {
                        Value::Arr(vec![Value::Num(*u), Value::Num(*v), Value::Num(*c as f64)])
                    })
                    .collect(),
            ),
        );
    }
    pack(&body.to_text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_area_coordinates_keep_their_full_accepted_precision() {
        let areas = [(0.123456789012345, 1e-9, 3), (-2.5000001, -0.0, 4)];
        let line = drawing_line("precise", &[super::super::custom::box_stroke()], &areas);
        let (_, _, reopened) = read_drawing(&unpack(&line).unwrap()).unwrap();
        assert_eq!(reopened, areas);
        assert_eq!(reopened[1].1.to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn share_lines_round_trip_and_spiderweb_lines_read() {
        let sh = json::parse(
            r#"{"kind": "line", "pts": [[0, 60], [1.0000000000001, 64]], "vel0": 100}"#,
        )
        .unwrap();
        let line = shapes_line(std::slice::from_ref(&sh));
        assert!(line.starts_with("SPIDERWEB1:"));
        let got = unpack(&format!("see: {line} (copy it)")).unwrap();
        let shapes = read_shapes(&got).unwrap();
        assert_eq!(shapes.len(), 1);
        assert_eq!(
            shapes[0].get("pts").unwrap().to_text(),
            "[[0, 60], [1.0000000000001, 64]]"
        );
        // made by Spiderweb (share.shapes_line of one line shape)
        let py = "SPIDERWEB1:eNqrVsrOzEtRslJQKs5ILEgtVtKBs6wUoquVylJzDIAsQwMDoASQYwjiGJkDOal5KfEp+SVAflpiTnEqUARmUk5mXirInIISsCHRQK1mBrE6CtGGQIZJbGxtbC0ACd0hkQ==";
        let s = read_shapes(&unpack(py).unwrap()).unwrap();
        assert_eq!(s[0].get("vel0").and_then(Value::as_f64), Some(100.0));
        let py = "SPIDERWEB1:eNqrVsrOzEtRslJKKUosz8xLV9JRykvMTQUKJOVXADnFJUX52anFSlbR1TCVBfk5lUCZghKQaLSBjkGsTrQhlDQEkiCR2NpYHaXEotREiBo9Ux0QNgaKAwDgWCHd";
        let (name, strokes, areas) = read_drawing(&unpack(py).unwrap()).unwrap();
        assert_eq!(
            (name.as_str(), strokes.len(), areas),
            ("box", 1, vec![(0.5, 0.5, 3)])
        );
        assert_eq!(unpack("nothing here"), Err(ShareError::None));
        assert_eq!(unpack("SPIDERWEB2:abcd"), Err(ShareError::Newer));
        assert_eq!(unpack("SPIDERWEB1:abc"), Err(ShareError::Broken));
        assert_eq!(unpack("SPIDERWEB1:YWJj=x"), Err(ShareError::Broken));
        let d = drawing_line(
            "box",
            &[super::super::custom::box_stroke()],
            &[(0.5, 0.5, 3)],
        );
        let (name, strokes, areas) = read_drawing(&unpack(&d).unwrap()).unwrap();
        assert_eq!(
            (name.as_str(), strokes.len(), areas),
            ("box", 1, vec![(0.5, 0.5, 3)])
        );
    }
}

#[cfg(test)]
mod print {
    #[test]
    #[ignore]
    fn print_a_share_line() {
        let sh = crate::json::parse(
            r#"{"kind": "curve", "pts": [[0, 60], [0.3, 70], [0.6, 50], [1, 60]], "vel0": 90}"#,
        )
        .unwrap();
        println!("LINE {}", super::shapes_line(&[sh]));
    }
}
