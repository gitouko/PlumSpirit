//! Original cubic contours from the shared native font pen, in compact columns.
use super::*;
use crate::app::spiderweb::fonts as native;
use plumspirit_engine::spiderweb::paths::P;

const MAX_POINTS: usize = 65536;
const MAX_CONTOURS: usize = 256;
const MAX_DRAW_BYTES: usize = 16 * 1024 * 1024;
fn limit(v: Value, default: usize, min: usize, max: usize) -> mlua::Result<usize> {
    match v {
        Value::Nil => Ok(default),
        Value::Integer(n) if n >= min as i64 && n <= max as i64 => Ok(n as usize),
        _ => Err(stop(
            Category::Budget,
            "Invalid glyph outline point/contour limit.",
        )),
    }
}
fn limits(opts: Option<Table>) -> mlua::Result<(usize, usize)> {
    let Some(opts) = opts else {
        return Ok((4096, 64));
    };
    if opts.metatable().is_some() {
        return Err(bad("Glyph outline options must be plain tables."));
    }
    for pair in opts.clone().pairs::<Value, Value>() {
        let (k, _) = pair?;
        if !matches!(k,Value::String(ref s) if s.as_bytes().len()<=12 && (s.to_str()?.as_ref()=="max_points" || s.to_str()?.as_ref()=="max_contours"))
        {
            return Err(bad("Unknown glyph outline option."));
        }
    }
    Ok((
        limit(opts.raw_get("max_points")?, 4096, 4, MAX_POINTS)?,
        limit(opts.raw_get("max_contours")?, 64, 1, MAX_CONTOURS)?,
    ))
}
fn native_error(e: native::OutlineError) -> mlua::Error {
    match e {
        native::OutlineError::Unsupported => bad(
            "Bounded glyph outlines currently require glyf; CFF/CFF2/VARC/bitmap-only outlines are unavailable.",
        ),
        native::OutlineError::Points => stop(
            Category::Budget,
            "Glyph outline exceeds max_points; no partial contour returned.",
        ),
        native::OutlineError::Contours => stop(
            Category::Budget,
            "Glyph outline exceeds max_contours; no partial contour returned.",
        ),
        native::OutlineError::Stopped => stop(Category::Canceled, "Glyph outline canceled."),
        native::OutlineError::Invalid => bad("Font produced invalid or nonfinite cubic contours."),
        native::OutlineError::Draw => bad("Native font outline could not be decoded safely."),
    }
}
fn outline_controlled(
    lua: &Lua,
    font: &Font,
    code: Value,
    opts: Option<Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let (max_points, max_contours) = limits(opts)?;
    font.with(lua, |_, face| {
        let metrics = face.metrics(Size::unscaled(), LocationRef::default());
        let id = index(code, u32::from(metrics.glyph_count) - 1)?;
        let needed = native::outline_memory(&face, id).map_err(native_error)?;
        if needed > MAX_DRAW_BYTES {
            return Err(stop(
                Category::Budget,
                "Glyph native draw scratch exceeds16MiB.",
            ));
        }
        let budget = lua
            .app_data_ref::<NativeBudget>()
            .ok_or_else(|| bad("Font accounting unavailable."))?
            .clone();
        let _workspace = budget
            .reserve(needed + 4 * max_points * std::mem::size_of::<P>() + max_contours * 128 + 1024)
            .map_err(|e| e.lua())?;
        let mut memory = Vec::new();
        memory
            .try_reserve_exact(needed)
            .map_err(|_| stop(Category::Memory, "Cannot allocate bounded glyph scratch."))?;
        memory.resize(needed, 0);
        let mut failure = None;
        let result = native::bounded_outline(
            &face,
            id,
            max_points,
            max_contours,
            &mut memory,
            &mut || match check() {
                Ok(()) => true,
                Err(e) => {
                    failure = Some(e);
                    false
                }
            },
        );
        if let Some(e) = failure {
            return Err(e);
        }
        let contours = result.map_err(native_error)?;
        check()?;
        let count = contours.iter().map(Vec::len).sum::<usize>();
        let out = lua.create_table()?;
        let (x, y) = (
            lua.create_table_with_capacity(count, 0)?,
            lua.create_table_with_capacity(count, 0)?,
        );
        let (first, last) = (
            lua.create_table_with_capacity(contours.len(), 0)?,
            lua.create_table_with_capacity(contours.len(), 0)?,
        );
        let (mut i, mut bounds) = (
            0usize,
            [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
        );
        for (c, points) in contours.iter().enumerate() {
            first.raw_set(c + 1, i + 1)?;
            for p in points {
                if i.is_multiple_of(64) {
                    check()?;
                }
                i += 1;
                x.raw_set(i, p[0])?;
                y.raw_set(i, p[1])?;
                bounds = [
                    bounds[0].min(p[0]),
                    bounds[1].min(p[1]),
                    bounds[2].max(p[0]),
                    bounds[3].max(p[1]),
                ];
            }
            last.raw_set(c + 1, i + 1)?;
        }
        out.raw_set("count", count)?;
        out.raw_set("contour_count", contours.len())?;
        out.raw_set("x", x)?;
        out.raw_set("y", y)?;
        out.raw_set("first", first)?;
        out.raw_set("last", last)?;
        out.raw_set("advance", advance(&face, id)?)?;
        out.raw_set("units", "em")?;
        out.raw_set("format", "cubic")?;
        out.raw_set("status", if count == 0 { "no_outline" } else { "outline" })?;
        if count != 0 {
            let b = lua.create_table()?;
            for (k, v) in ["x_min", "y_min", "x_max", "y_max"].into_iter().zip(bounds) {
                b.raw_set(k, v)?;
            }
            out.raw_set("control_bounds", b)?;
        }
        check()?;
        Ok(out)
    })
}
pub(super) fn add_methods<M: UserDataMethods<Font>>(m: &mut M) {
    m.add_method(
        "glyph_outline",
        |lua, this, (code, opts): (Value, Option<Table>)| {
            outline_controlled(lua, this, code, opts, || notes::check_budget(lua))
        },
    );
}
pub(super) fn install(t: &Table) -> mlua::Result<()> {
    t.set("max_glyph_points", MAX_POINTS)?;
    t.set("max_glyph_contours", MAX_CONTOURS)?;
    t.set("max_glyph_draw_bytes", MAX_DRAW_BYTES)
}

#[cfg(test)]
#[path = "text_font_outline_tests.rs"]
mod tests;
