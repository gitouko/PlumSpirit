//! Bounded single-run shaping using egui's already-locked Harfrust engine.
use super::*;
use harfrust::{Direction, Feature, Language, Script};
use unicode_segmentation::UnicodeSegmentation;

const MAX_TEXT: usize = 4096;
const MAX_SCALARS: usize = 256;
const MAX_GLYPHS: usize = 4096;
// Harfrust's native buffer caps at max(scalars*256,65536) info/position pairs.
// Charge twice that capacity plus trusted font caches/plan/scratch, all transient.
const WORKSPACE: usize = 32 * 1024 * 1024;
struct Options {
    direction: Option<Direction>,
    script: Option<Script>,
    language: Option<Language>,
    features: Vec<Feature>,
    cap: usize,
}
fn text(v: Value, max: usize) -> mlua::Result<String> {
    match v {
        Value::String(s) if s.as_bytes().len() <= max => Ok(s.to_str()?.to_string()),
        _ => Err(bad(
            "Shaping expects bounded UTF-8 text, not coercible values.",
        )),
    }
}
fn options(t: Option<Table>) -> mlua::Result<Options> {
    let mut out = Options {
        direction: None,
        script: None,
        language: None,
        features: Vec::new(),
        cap: MAX_GLYPHS,
    };
    let Some(t) = t else {
        return Ok(out);
    };
    if t.metatable().is_some() {
        return Err(bad("Shaping options must be plain tables."));
    }
    for p in t.clone().pairs::<Value, Value>() {
        let (k, _) = p?;
        if !matches!(k,Value::String(ref s) if s.as_bytes().len()<=10 && ["direction","script","language","features","max_glyphs"].contains(&s.to_str()?.as_ref()))
        {
            return Err(bad("Unknown shaping option."));
        }
    }
    if let v @ Value::String(_) = t.raw_get::<Value>("direction")? {
        out.direction = match text(v, 4)?.as_str() {
            "auto" => None,
            "ltr" => Some(Direction::LeftToRight),
            "rtl" => Some(Direction::RightToLeft),
            "ttb" => Some(Direction::TopToBottom),
            "btt" => Some(Direction::BottomToTop),
            _ => return Err(bad("Shaping direction must be auto, ltr, rtl, ttb or btt.")),
        };
    } else if !matches!(t.raw_get::<Value>("direction")?, Value::Nil) {
        return Err(bad("Shaping direction must be text."));
    }
    if !matches!(t.raw_get::<Value>("script")?, Value::Nil) {
        let s = text(t.raw_get("script")?, 4)?;
        if s.len() != 4 || !s.bytes().all(|c| c.is_ascii_alphabetic()) {
            return Err(bad("Script must be a four-letter ISO15924 tag."));
        }
        out.script = Some(s.parse().map_err(|_| bad("Invalid shaping script."))?);
    }
    if !matches!(t.raw_get::<Value>("language")?, Value::Nil) {
        let s = text(t.raw_get("language")?, 63)?;
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return Err(bad("Language must be a bounded ASCII language tag."));
        }
        out.language = Some(s.parse().map_err(|_| bad("Invalid shaping language."))?);
    }
    match t.raw_get::<Value>("max_glyphs")? {
        Value::Nil => {}
        Value::Integer(n) if (1..=MAX_GLYPHS as i64).contains(&n) => out.cap = n as usize,
        _ => {
            return Err(stop(
                Category::Budget,
                "Shaping max_glyphs must be an integer1–4096.",
            ));
        }
    }
    match t.raw_get::<Value>("features")? {
        Value::Nil => {}
        Value::Table(t) if t.metatable().is_none() => {
            for p in t.pairs::<Value, Value>() {
                if out.features.len() == 16 {
                    return Err(stop(
                        Category::Budget,
                        "At most16shaping features are allowed.",
                    ));
                }
                let (k, v) = p?;
                let tag = text(k, 4)?;
                if tag.len() != 4 || !tag.bytes().all(|c| c.is_ascii_alphanumeric() || c == b' ') {
                    return Err(bad("Feature tags must be four ASCII characters."));
                }
                let value = match v {
                    Value::Boolean(b) => u32::from(b),
                    Value::Integer(n) if (0..=65535).contains(&n) => n as u32,
                    _ => return Err(bad("Feature values must be booleans or integers0–65535.")),
                };
                out.features.push(Feature {
                    tag: harfrust::Tag::new(tag.as_bytes().try_into().unwrap()),
                    value,
                    start: 0,
                    end: u32::MAX,
                });
            }
            out.features.sort_unstable_by_key(|f| f.tag);
        }
        _ => return Err(bad("Shaping features must be a plain tag/value table.")),
    }
    Ok(out)
}
fn direction_name(d: Direction) -> &'static str {
    match d {
        Direction::RightToLeft => "rtl",
        Direction::TopToBottom => "ttb",
        Direction::BottomToTop => "btt",
        _ => "ltr",
    }
}
fn shape_controlled(
    lua: &Lua,
    font: &Font,
    value: Value,
    opts: Option<Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let source = text(value, MAX_TEXT)?;
    if source.chars().count() > MAX_SCALARS {
        return Err(stop(
            Category::Budget,
            "A shaped run allows at most256Unicode scalars; split at appropriate run boundaries.",
        ));
    }
    if source.contains(['\r', '\n', '\u{2028}', '\u{2029}']) {
        return Err(bad(
            "shape_run accepts one run; split line and paragraph boundaries first.",
        ));
    }
    let opts = options(opts)?;
    font.with(lua, |data, face| {
        shape_face(lua, data, face, &source, &opts, &mut check)
    })
}
fn native_error(message: String) -> mlua::Error {
    if message == "Shaped output exceeds max_glyphs."
        || message
            == "Native font shaping exhausted its operation or buffer limits; no partial run returned."
    {
        stop(Category::Budget, message)
    } else if message.starts_with("Cannot allocate ") {
        stop(Category::Memory, message)
    } else {
        bad(&message)
    }
}
fn shape_face(
    lua: &Lua,
    data: &Stored,
    face: FontRef<'_>,
    source: &str,
    opts: &Options,
    check: &mut impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    let bundled = data.kind == "builtin";
    if !bundled {
        let mut slot = lua
            .app_data_mut::<FontSlot>()
            .ok_or_else(|| bad("Font resource scope unavailable."))?;
        if slot.1 >= 8 {
            return Err(stop(
                Category::Budget,
                "At most8external font shaping attempts are allowed per job.",
            ));
        }
        slot.1 += 1;
    }
    let _native = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Font accounting unavailable."))?
        .reserve(if bundled {
            WORKSPACE
        } else {
            crate::font_shape::RESERVATION_BYTES
        })
        .map_err(|e| e.lua())?;
    let metrics = face.metrics(Size::unscaled(), LocationRef::default());
    let em = f64::from(metrics.units_per_em);
    let request = crate::font_shape::Request {
        face: data.index,
        text: source.to_owned(),
        direction: opts.direction.map(|d| direction_name(d).to_owned()),
        script: opts.script.map(|s| s.tag().to_string()),
        language: opts.language.as_ref().map(|l| l.as_str().to_owned()),
        features: opts
            .features
            .iter()
            .map(|f| (f.tag.to_string(), f.value))
            .collect(),
        cap: opts.cap,
    };
    let shaped = if bundled {
        // Preserve cheap cancellation while walking trusted input and immediately
        // before/after its bounded native phase; no Lua enters the shaper.
        for _ in source.graphemes(true) {
            check()?;
        }
        check()?;
        let result = crate::font_shape::shape(&request, &data.bytes).map_err(native_error)?;
        check()?;
        result
    } else {
        crate::font_shape::run(&request,&data.bytes,&mut *check).map_err(|e|match e {
            crate::font_shape::Error::Stopped(e)=>e,
            crate::font_shape::Error::Timeout=>stop(Category::TimeLimit,"External font shaping exceeded its1500ms isolated worker limit; use shorter runs or another font."),
            crate::font_shape::Error::Refused(e)=>bad(e),
            crate::font_shape::Error::Native(e)=>native_error(e),
        })?
    };
    if shaped.glyphs.len() > opts.cap {
        return Err(stop(
            Category::Budget,
            "Shaped output exceeds max_glyphs; no partial run returned.",
        ));
    }
    let mut ends = shaped
        .glyphs
        .iter()
        .map(|g| g.cluster as usize)
        .collect::<Vec<_>>();
    ends.push(source.len());
    ends.sort_unstable();
    ends.dedup();
    let out = lua.create_table()?;
    let names = [
        "glyph_id",
        "first",
        "last",
        "x",
        "y",
        "x_advance",
        "y_advance",
        "x_offset",
        "y_offset",
        "missing",
        "unsafe_to_break",
    ];
    let cols = names
        .iter()
        .map(|_| lua.create_table_with_capacity(shaped.glyphs.len(), 0))
        .collect::<mlua::Result<Vec<_>>>()?;
    let (mut x, mut y, mut missing) = (0., 0., 0usize);
    for (i, info) in shaped.glyphs.iter().enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        let first = info.cluster as usize;
        if info.id >= u32::from(metrics.glyph_count)
            || first >= source.len()
            || !source.is_char_boundary(first)
        {
            return Err(bad(
                "Native shaping returned invalid glyph or cluster data.",
            ));
        }
        let end = *ends
            .get(ends.binary_search(&first).unwrap() + 1)
            .ok_or_else(|| bad("Native shaping cluster end is invalid."))?;
        let (xa, ya, xo, yo) = (
            f64::from(info.xa) / em,
            f64::from(info.ya) / em,
            f64::from(info.xo) / em,
            f64::from(info.yo) / em,
        );
        if [xa, ya, xo, yo, x + xo, y + yo]
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1e6)
        {
            return Err(bad("Native shaping positions are invalid."));
        }
        cols[0].raw_set(i + 1, info.id)?;
        cols[1].raw_set(i + 1, first + 1)?;
        cols[2].raw_set(i + 1, end + 1)?;
        for (col, v) in cols[3..9].iter().zip([x + xo, y + yo, xa, ya, xo, yo]) {
            col.raw_set(i + 1, v)?;
        }
        cols[9].raw_set(i + 1, info.id == 0)?;
        cols[10].raw_set(i + 1, info.unsafe_break)?;
        missing += usize::from(info.id == 0);
        x += xa;
        y += ya;
    }
    check()?;
    if !x.is_finite() || !y.is_finite() || x.abs() > 1e6 || y.abs() > 1e6 {
        return Err(bad("Native shaping total advance is invalid."));
    }
    for (name, col) in names.iter().zip(cols) {
        out.raw_set(*name, col)?;
    }
    out.raw_set("count", shaped.glyphs.len())?;
    out.raw_set("text", source)?;
    out.raw_set("units", "em")?;
    out.raw_set("direction", shaped.direction)?;
    out.raw_set("script", shaped.script)?;
    out.raw_set("language", shaped.language)?;
    out.raw_set("advance_x", x)?;
    out.raw_set("advance_y", y)?;
    out.raw_set("missing_count", missing)?;
    Ok(out)
}

pub(super) fn add_methods<M: UserDataMethods<Font>>(m: &mut M) {
    m.add_method(
        "shape_run",
        |lua, this, (text, opts): (Value, Option<Table>)| {
            shape_controlled(lua, this, text, opts, || notes::check_budget(lua))
        },
    );
}
pub(super) fn install(t: &Table) -> mlua::Result<()> {
    for (key, value) in [
        ("max_external_shape_calls", 8),
        (
            "max_external_shape_millis",
            crate::font_shape::MAX_MILLIS as usize,
        ),
        (
            "external_shape_memory_bytes",
            crate::font_shape::MEMORY_BYTES,
        ),
        ("max_shape_bytes", MAX_TEXT),
        ("max_shape_scalars", MAX_SCALARS),
        ("max_shaped_glyphs", MAX_GLYPHS),
    ] {
        t.set(key, value)?;
    }
    Ok(())
}
#[cfg(test)]
#[path = "text_font_shaping_tests.rs"]
mod tests;
