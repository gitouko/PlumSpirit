//! Bounded original-byte bidi metadata and grapheme-safe line-break opportunities.
use super::*;
use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo};
use unicode_segmentation::UnicodeSegmentation;

#[path = "text_script.rs"]
mod script;

const MAX_BYTES: usize = 16384;
const MAX_SCALARS: usize = 4096;
const MAX_LINES: usize = 256;
const MAX_RUNS: usize = 4096;

fn bad(s: &str) -> mlua::Error {
    stop(Category::Runtime, s)
}
pub(super) fn keys(t: &Table, allowed: &[&str]) -> mlua::Result<()> {
    if t.metatable().is_some() {
        return Err(bad("Text layout options must be plain tables."));
    }
    for p in t.clone().pairs::<Value, Value>() {
        let (k, _) = p?;
        if !matches!(k,Value::String(s)if s.as_bytes().len()<=32 && allowed.contains(&s.to_str()?.as_ref()))
        {
            return Err(bad("Unknown text layout option."));
        }
    }
    Ok(())
}
pub(super) fn limit(v: Value, default: usize, max: usize) -> mlua::Result<usize> {
    match v {
        Value::Nil => Ok(default),
        Value::Integer(n) if n >= 1 && n <= max as i64 => Ok(n as usize),
        _ => Err(stop(
            Category::Budget,
            "Text layout limits must be bounded positive Lua integers.",
        )),
    }
}
pub(super) fn input(
    v: Value,
    check: &mut impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<BorrowedStr> {
    let Value::String(s) = v else {
        return Err(bad("Text layout expects a UTF-8 string."));
    };
    if s.as_bytes().len() > MAX_BYTES {
        return Err(stop(
            Category::Budget,
            "Text layout exceeds16384UTF-8 bytes.",
        ));
    }
    let s = s
        .to_str()
        .map_err(|_| bad("Text layout input is invalid UTF-8; bytes were not replaced."))?;
    for (i, _) in s.chars().enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        if i >= MAX_SCALARS {
            return Err(stop(
                Category::Budget,
                "Text layout exceeds4096Unicode scalars.",
            ));
        }
    }
    Ok(s)
}
pub(super) fn workspace(lua: &Lua, bytes: usize) -> mlua::Result<Reservation> {
    lua.app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Text layout accounting unavailable."))?
        .reserve(bytes * 128 + 512 * 1024)
        .map_err(|e| e.lua())
}
fn boundaries(s: &str, check: &mut impl FnMut() -> mlua::Result<()>) -> mlua::Result<Vec<bool>> {
    let mut out = Vec::new();
    out.try_reserve_exact(s.len() + 1)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded text boundaries."))?;
    out.resize(s.len() + 1, false);
    out[s.len()] = true;
    for (i, (offset, _)) in s.grapheme_indices(true).enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        out[offset] = true;
    }
    Ok(out)
}
#[derive(Clone, Copy)]
struct Line {
    first: usize,
    content_last: usize,
    last: usize,
}
fn lines(
    s: &str,
    cap: usize,
    check: &mut impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Vec<Line>> {
    let mut lines = Vec::new();
    lines
        .try_reserve_exact(cap)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded text lines."))?;
    let mut first = 0;
    let mut chars = s.char_indices().peekable();
    let mut count = 0usize;
    let push = |lines: &mut Vec<Line>, line| -> mlua::Result<()> {
        if lines.len() == cap {
            return Err(stop(
                Category::Budget,
                "Text layout exceeds max_lines; no partial analysis returned.",
            ));
        }
        lines.push(line);
        Ok(())
    };
    while let Some((offset, c)) = chars.next() {
        if count.is_multiple_of(64) {
            check()?;
        }
        count += 1;
        if unicode_bidi::bidi_class(c) == BidiClass::B
            || matches!(c, '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}')
        {
            let mut last = offset + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|&(_, c)| c == '\n') {
                let (i, c) = chars.next().unwrap();
                last = i + c.len_utf8();
                count += 1;
            }
            push(
                &mut lines,
                Line {
                    first,
                    content_last: offset,
                    last,
                },
            )?;
            first = last;
        }
    }
    push(
        &mut lines,
        Line {
            first,
            content_last: s.len(),
            last: s.len(),
        },
    )?;
    Ok(lines)
}
fn bidi_controlled(
    lua: &Lua,
    value: Value,
    opts: Option<Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let (mut base, mut run_cap, mut line_cap) = (None, MAX_RUNS, MAX_LINES);
    if let Some(t) = opts {
        keys(&t, &["direction", "max_runs", "max_lines"])?;
        base = match t.raw_get::<Value>("direction")? {
            Value::Nil => None,
            Value::String(s) if s.as_bytes().len() <= 4 => match s.to_str()?.as_ref() {
                "auto" => None,
                "ltr" => Some(Level::ltr()),
                "rtl" => Some(Level::rtl()),
                _ => return Err(bad("Bidi direction must be auto, ltr or rtl.")),
            },
            _ => return Err(bad("Bidi direction must be bounded text.")),
        };
        run_cap = limit(t.raw_get("max_runs")?, MAX_RUNS, MAX_RUNS)?;
        line_cap = limit(t.raw_get("max_lines")?, MAX_LINES, MAX_LINES)?;
    }
    let source = input(value, &mut check)?;
    let _workspace = workspace(lua, source.len())?;
    let boundary = boundaries(&source, &mut check)?;
    let lines = lines(&source, line_cap, &mut check)?;
    let result = lua.create_table()?;
    let line_tables = lua.create_table_with_capacity(lines.len(), 0)?;
    let names = ["first", "last", "line", "level", "direction"];
    let cols = names
        .iter()
        .map(|_| lua.create_table())
        .collect::<mlua::Result<Vec<_>>>()?;
    let mut count = 0usize;
    let mut has_rtl = false;
    for (index, line) in lines.iter().enumerate() {
        check()?;
        let content = &source[line.first..line.content_last];
        let bidi = ParagraphBidiInfo::new(content, base);
        check()?;
        let info = lua.create_table()?;
        info.raw_set("first", line.first + 1)?;
        info.raw_set("content_last", line.content_last + 1)?;
        info.raw_set("last", line.last + 1)?;
        info.raw_set("level", bidi.paragraph_level.number())?;
        info.raw_set(
            "direction",
            if bidi.paragraph_level.is_rtl() {
                "rtl"
            } else {
                "ltr"
            },
        )?;
        info.raw_set("first_run", count + 1)?;
        has_rtl |= bidi.paragraph_level.is_rtl();
        if !content.is_empty() {
            let (levels, runs) = bidi.visual_runs(0..content.len());
            check()?;
            for (i, range) in runs.iter().enumerate() {
                if i.is_multiple_of(64) {
                    check()?;
                }
                if count == run_cap {
                    return Err(stop(
                        Category::Budget,
                        "Bidi analysis exceeds max_runs; no partial analysis returned.",
                    ));
                }
                let (first, last) = (line.first + range.start, line.first + range.end);
                if first >= last || !boundary[first] || !boundary[last] {
                    return Err(bad(
                        "Native bidi boundary splits a grapheme; use explicit authored run boundaries.",
                    ));
                }
                let level = levels[range.start];
                let n = count + 1;
                cols[0].raw_set(n, first + 1)?;
                cols[1].raw_set(n, last + 1)?;
                cols[2].raw_set(n, index + 1)?;
                cols[3].raw_set(n, level.number())?;
                cols[4].raw_set(n, if level.is_rtl() { "rtl" } else { "ltr" })?;
                has_rtl |= level.is_rtl();
                count += 1;
            }
        }
        info.raw_set("last_run", count + 1)?;
        line_tables.raw_set(index + 1, info)?;
    }
    check()?;
    for (name, col) in names.iter().zip(cols) {
        result.raw_set(*name, col)?;
    }
    result.raw_set("count", count)?;
    result.raw_set("lines", line_tables)?;
    result.raw_set("line_count", lines.len())?;
    result.raw_set("has_rtl", has_rtl)?;
    result.raw_set("text", source.as_ref())?;
    Ok(result)
}
fn breaks_controlled(
    lua: &Lua,
    value: Value,
    opts: Option<Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let cap = if let Some(t) = opts {
        keys(&t, &["max_breaks"])?;
        limit(t.raw_get("max_breaks")?, MAX_RUNS, MAX_RUNS)?
    } else {
        MAX_RUNS
    };
    let source = input(value, &mut check)?;
    let _workspace = workspace(lua, source.len())?;
    let boundary = boundaries(&source, &mut check)?;
    let result = lua.create_table()?;
    let positions = lua.create_table()?;
    let kinds = lua.create_table()?;
    let mut count = 0usize;
    for (i, (position, kind)) in unicode_linebreak::linebreaks(&source).enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        if !boundary[position] {
            continue;
        }
        if count == cap {
            return Err(stop(
                Category::Budget,
                "Line breaks exceed max_breaks; no partial analysis returned.",
            ));
        }
        count += 1;
        positions.raw_set(count, position + 1)?;
        kinds.raw_set(
            count,
            match kind {
                unicode_linebreak::BreakOpportunity::Allowed => "allowed",
                unicode_linebreak::BreakOpportunity::Mandatory => "mandatory",
            },
        )?;
    }
    check()?;
    result.raw_set("count", count)?;
    result.raw_set("position", positions)?;
    result.raw_set("kind", kinds)?;
    result.raw_set("text", source.as_ref())?;
    Ok(result)
}
pub(super) fn install(lua: &Lua, text: &Table) -> mlua::Result<()> {
    let (a, b, c) = unicode_bidi::UNICODE_VERSION;
    text.set("bidi_unicode_version", format!("{a}.{b}.{c}"))?;
    let (a, b, c) = unicode_linebreak::UNICODE_VERSION;
    text.set("linebreak_unicode_version", format!("{a}.{b}.{c}"))?;
    for (key, value) in [
        ("max_layout_bytes", MAX_BYTES),
        ("max_layout_scalars", MAX_SCALARS),
        ("max_layout_lines", MAX_LINES),
        ("max_layout_runs", MAX_RUNS),
    ] {
        text.set(key, value)?;
    }
    text.set(
        "bidi_runs",
        lua.create_function(|lua, (value, opts): (Value, Option<Table>)| {
            bidi_controlled(lua, value, opts, || notes::check_budget(lua))
        })?,
    )?;
    text.set(
        "line_breaks",
        lua.create_function(|lua, (value, opts): (Value, Option<Table>)| {
            breaks_controlled(lua, value, opts, || notes::check_budget(lua))
        })?,
    )?;
    script::install(lua, text)
}
#[cfg(test)]
#[path = "text_layout_tests.rs"]
mod tests;
