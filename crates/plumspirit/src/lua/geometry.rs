//! Bounded pure geometry sharing the native drawing and image affine services.
use super::bridge::{NativeBudget, Reservation};
use super::{Category, stop};
use mlua::{Lua, Table, Value};
use plumspirit_engine::transform::generator::draw::{self, Pt};

pub const MAX_POINTS: usize = 4096;
/// Canonical zero-based cell coverage shared with the A8 image service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RasterSpan {
    pub row: u32,
    pub first: u32,
    pub last: u32,
}
pub(super) fn mask_spans(
    lua: &Lua,
    input: &Table,
    width: u32,
    height: u32,
    check: &mut impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<(Vec<RasterSpan>, Reservation)> {
    raster::mask_spans(lua, input, width, height, check)
}
#[path = "geometry_math.rs"]
mod creative_math;
#[path = "geometry_curves.rs"]
mod curves;
#[path = "geometry_predicates.rs"]
mod predicates;
#[path = "geometry_raster.rs"]
mod raster;
fn bad(s: &str) -> mlua::Error {
    stop(Category::Runtime, s)
}
fn keys(t: &Table, allowed: &[&str]) -> mlua::Result<()> {
    if t.metatable().is_some() {
        return Err(bad("Geometry descriptors must be plain tables."));
    }
    for pair in t.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        let Value::String(key) = key else {
            return Err(bad("Geometry descriptor keys must be text."));
        };
        if !allowed.contains(&key.to_str()?.as_ref()) {
            return Err(bad("Unknown geometry descriptor field."));
        }
    }
    Ok(())
}
fn coordinate(v: Value) -> mlua::Result<f64> {
    let n = match v {
        Value::Integer(n) => n as f64,
        Value::Number(n) => n,
        _ => return Err(bad("Geometry coordinates must be numbers.")),
    };
    if !n.is_finite() || n.abs() > 1e12 {
        return Err(bad(
            "Geometry coordinates must be finite within +/-1e12; use local offsets for large ticks.",
        ));
    }
    Ok(n)
}
fn array(lua: &Lua, t: &Table, n: usize) -> mlua::Result<()> {
    if t.metatable().is_some() {
        return Err(bad("Geometry columns must be plain dense arrays."));
    }
    let mut count = 0;
    for p in t.clone().pairs::<Value, Value>() {
        let (k, _) = p?;
        count += 1;
        if count % 64 == 0 {
            super::notes::check_budget(lua)?;
        }
        if count > n || !matches!(k,Value::Integer(i) if i>=1 && i as usize<=n) {
            return Err(bad("Geometry columns must be plain dense arrays."));
        }
    }
    if count != n {
        return Err(bad("Geometry column contains holes."));
    }
    Ok(())
}
fn points(lua: &Lua, t: &Table, output: usize) -> mlua::Result<(Vec<Pt>, Reservation)> {
    super::notes::check_budget(lua)?;
    keys(t, &["x", "y", "count"])?;
    let x = t.raw_get::<Table>("x")?;
    let y = t.raw_get::<Table>("y")?;
    let n = x.raw_len();
    if n == 0 || n > MAX_POINTS || y.raw_len() != n {
        return Err(stop(
            Category::Budget,
            "Geometry needs equal x/y columns with1–4096 points.",
        ));
    }
    match t.raw_get::<Value>("count")? {
        Value::Nil => {}
        Value::Integer(count) if count == n as i64 => {}
        _ => {
            return Err(bad(
                "Geometry count must be an integer matching its columns.",
            ));
        }
    }
    array(lua, &x, n)?;
    array(lua, &y, n)?;
    let native = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Geometry accounting unavailable."))?
        .reserve((n + output) * std::mem::size_of::<Pt>() + 1024)
        .map_err(|e| e.lua())?;
    let mut p = Vec::new();
    p.try_reserve_exact(n)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded geometry."))?;
    for i in 1..=n {
        if i % 64 == 0 {
            super::notes::check_budget(lua)?;
        }
        p.push(Pt {
            x: coordinate(x.raw_get(i)?)?,
            y: coordinate(y.raw_get(i)?)?,
            s: (i - 1) as f64 / (n - 1).max(1) as f64,
        });
    }
    Ok((p, native))
}
fn columns(lua: &Lua, p: &[Pt]) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    let x = lua.create_table_with_capacity(p.len(), 0)?;
    let y = lua.create_table_with_capacity(p.len(), 0)?;
    for (i, p) in p.iter().enumerate() {
        if i % 64 == 0 {
            super::notes::check_budget(lua)?;
        }
        x.raw_set(i + 1, p.x)?;
        y.raw_set(i + 1, p.y)?;
    }
    out.raw_set("count", p.len())?;
    out.raw_set("x", x)?;
    out.raw_set("y", y)?;
    Ok(out)
}
fn window(t: &Table) -> mlua::Result<[f64; 4]> {
    keys(t, &["x_min", "y_min", "x_max", "y_max"])?;
    let w = [
        coordinate(t.raw_get("x_min")?)?,
        coordinate(t.raw_get("y_min")?)?,
        coordinate(t.raw_get("x_max")?)?,
        coordinate(t.raw_get("y_max")?)?,
    ];
    if w[0] > w[2] || w[1] > w[3] {
        return Err(bad("Geometry box minima must not exceed maxima."));
    }
    Ok(w)
}
pub(super) fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let g = lua.create_table()?;
    creative_math::install(lua, &g)?;
    // The exact same implementation/schema as images.mapping/compose/inverse/map_point.
    super::images::mapping::install(lua, &g)?;
    g.set("max_points", MAX_POINTS)?;
    g.set(
        "map_points",
        lua.create_function(|lua, (matrix, t): (Table, Table)| {
            let m = super::images::mapping::read(&matrix)?;
            let n = t.raw_get::<Table>("x")?.raw_len();
            let (mut p, _native) = points(lua, &t, n)?;
            for (i, p) in p.iter_mut().enumerate() {
                if i % 64 == 0 {
                    super::notes::check_budget(lua)?;
                }
                let (x, y) = super::images::mapping::checked_point(m.xy((p.x, p.y)))?;
                p.x = x;
                p.y = y;
            }
            columns(lua, &p)
        })?,
    )?;
    g.set(
        "contains",
        lua.create_function(|lua, (w, x, y): (Table, Value, Value)| {
            super::notes::check_budget(lua)?;
            Ok(draw::inside(&window(&w)?, (coordinate(x)?, coordinate(y)?)))
        })?,
    )?;
    g.set(
        "clip_segment",
        lua.create_function(
            |lua, (w, x0, y0, x1, y1): (Table, Value, Value, Value, Value)| {
                super::notes::check_budget(lua)?;
                let w = window(&w)?;
                let p = (coordinate(x0)?, coordinate(y0)?);
                let q = (coordinate(x1)?, coordinate(y1)?);
                let Some((a, b)) = draw::clip_seg(&w, p, q) else {
                    return Ok(None);
                };
                let t = lua.create_table()?;
                for (k, v) in [
                    ("x0", p.0 + (q.0 - p.0) * a),
                    ("y0", p.1 + (q.1 - p.1) * a),
                    ("x1", p.0 + (q.0 - p.0) * b),
                    ("y1", p.1 + (q.1 - p.1) * b),
                    ("t0", a),
                    ("t1", b),
                ] {
                    t.raw_set(k, v)?;
                }
                Ok(Some(t))
            },
        )?,
    )?;
    g.set(
        "polyline_length",
        lua.create_function(|lua, t: Table| {
            let (p, _native) = points(lua, &t, 0)?;
            let n = draw::length(&p);
            super::notes::check_budget(lua)?;
            Ok(n)
        })?,
    )?;
    g.set(
        "resample_polyline",
        lua.create_function(|lua, (t, count): (Table, Value)| {
            let segments = match count {
                Value::Integer(n) if n >= 1 && n < MAX_POINTS as i64 => n as usize,
                _ => {
                    return Err(stop(
                        Category::Budget,
                        "Resampling needs an integer1–4095 segments.",
                    ));
                }
            };
            let (p, _native) = points(lua, &t, segments + 1)?;
            // Native linear walk, at most4096 input and4096 output points; bounded phase.
            let p = draw::resample(&p, segments);
            super::notes::check_budget(lua)?;
            columns(lua, &p)
        })?,
    )?;
    curves::install(lua, &g)?;
    predicates::install(lua, &g)?;
    raster::install(lua, &g)?;
    plum.set("geometry", g)
}

#[cfg(test)]
#[path = "geometry_tests.rs"]
mod tests;
