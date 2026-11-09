//! Pure bounded worker-side hit testing. No project scan or Lua evaluator.
use super::*;
use draw::geometry::nearest_segment;
use plumspirit_engine::transform::generator::area;

fn boolean(v: Value) -> mlua::Result<bool> {
    match v {
        Value::Nil => Ok(false),
        Value::Boolean(b) => Ok(b),
        _ => Err(bad("Polyline closed must be a boolean.")),
    }
}
fn classify_controlled(
    lua: &Lua,
    t: &Table,
    x: Value,
    y: Value,
    opts: Option<Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<&'static str> {
    check()?;
    let q = (coordinate(x)?, coordinate(y)?);
    let (nonzero, epsilon) = if let Some(opts) = opts {
        keys(&opts, &["rule", "epsilon"])?;
        let rule = match opts.raw_get::<Value>("rule")? {
            Value::Nil => false,
            Value::String(s) if s.to_str()? == "even_odd" => false,
            Value::String(s) if s.to_str()? == "non_zero" => true,
            _ => return Err(bad("Polygon rule must be even_odd or non_zero.")),
        };
        let epsilon = match opts.raw_get::<Value>("epsilon")? {
            Value::Nil => 0.0,
            v => coordinate(v)?,
        };
        if epsilon < 0.0 {
            return Err(bad("Polygon epsilon must be nonnegative."));
        }
        (rule, epsilon)
    } else {
        (false, 0.0)
    };
    let (p, _native) = points(lua, t, 0)?;
    if p.len() < 3 {
        return Err(bad("A polygon needs at least three points."));
    }
    let mut winding = 0i32;
    for i in 0..p.len() {
        if i.is_multiple_of(64) {
            check()?;
        }
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        let (a, b) = ((a.x, a.y), (b.x, b.y));
        if nearest_segment(a, b, q).2 <= epsilon {
            return Ok("boundary");
        }
        winding += area::ray_winding(a, b, q);
    }
    check()?;
    let inside = if nonzero {
        winding != 0
    } else {
        winding & 1 != 0
    };
    Ok(if inside { "inside" } else { "outside" })
}
fn nearest_controlled(
    lua: &Lua,
    t: &Table,
    q: (f64, f64),
    closed: bool,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let (p, _native) = points(lua, t, 0)?;
    let mut best = ((p[0].x, p[0].y), 0.0, f64::INFINITY, 1usize);
    let edges = if closed {
        p.len()
    } else {
        p.len().saturating_sub(1).max(1)
    };
    for i in 0..edges {
        if i.is_multiple_of(64) {
            check()?;
        }
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        let (point, t, d) = nearest_segment((a.x, a.y), (b.x, b.y), q);
        if d < best.2 {
            best = (point, t, d, i + 1);
        }
    }
    check()?;
    let out = lua.create_table()?;
    for (k, v) in [
        ("x", best.0.0),
        ("y", best.0.1),
        ("t", best.1),
        ("distance", best.2),
    ] {
        out.raw_set(k, v)?;
    }
    out.raw_set("segment", best.3)?;
    Ok(out)
}
pub(super) fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    g.set(
        "polygon_classify",
        lua.create_function(
            |lua, (t, x, y, opts): (Table, Value, Value, Option<Table>)| {
                classify_controlled(lua, &t, x, y, opts, || crate::lua::notes::check_budget(lua))
            },
        )?,
    )?;
    g.set(
        "nearest_polyline",
        lua.create_function(
            |lua, (t, x, y, opts): (Table, Value, Value, Option<Table>)| {
                let q = (coordinate(x)?, coordinate(y)?);
                let closed = if let Some(opts) = opts {
                    keys(&opts, &["closed"])?;
                    boolean(opts.raw_get("closed")?)?
                } else {
                    false
                };
                nearest_controlled(lua, &t, q, closed, || crate::lua::notes::check_budget(lua))
            },
        )?,
    )
}

#[cfg(test)]
#[path = "geometry_predicate_tests.rs"]
mod tests;
