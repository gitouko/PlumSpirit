//! Bounded adapters for the existing native generator's spline evaluators.
use super::*;
use plumspirit_engine::transform::generator::arcs;

#[derive(Clone, Copy)]
enum Kind {
    CatmullRom,
    BSpline,
}
impl Kind {
    fn read(v: Value) -> mlua::Result<Self> {
        let Value::String(s) = v else {
            return Err(bad("Spline kind must be catmull_rom or b_spline."));
        };
        if s.as_bytes().len() > 32 {
            return Err(bad("Spline kind must be bounded text."));
        }
        match s.to_str()?.as_ref() {
            "catmull_rom" => Ok(Self::CatmullRom),
            "b_spline" => Ok(Self::BSpline),
            _ => Err(bad("Spline kind must be catmull_rom or b_spline.")),
        }
    }
    fn at(self, p: &[Point], t: f64) -> mlua::Result<Point> {
        let (x, y) = match self {
            Self::CatmullRom => arcs::catmull(p, t),
            Self::BSpline => arcs::bspline(p, t),
        };
        if !x.is_finite() || !y.is_finite() || x.abs() > 1e12 || y.abs() > 1e12 {
            return Err(bad(
                "Spline output exceeds finite +/-1e12; use local offsets or smaller control coordinates.",
            ));
        }
        Ok((x, y))
    }
}
struct Input {
    xy: Vec<Point>,
    _source: Reservation,
    _workspace: Reservation,
}
fn read(lua: &Lua, t: &Table, output: usize) -> mlua::Result<Input> {
    let (p, source) = points(lua, t, output)?;
    let workspace = reserve(lua, p.len() * std::mem::size_of::<Point>() + 1024)?;
    let mut xy = Vec::new();
    xy.try_reserve_exact(p.len())
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded spline controls."))?;
    for (i, p) in p.iter().enumerate() {
        if i.is_multiple_of(64) {
            crate::lua::notes::check_budget(lua)?;
        }
        xy.push((p.x, p.y));
    }
    Ok(Input {
        xy,
        _source: source,
        _workspace: workspace,
    })
}
fn evaluate_controlled(
    lua: &Lua,
    t: &Table,
    kind: Kind,
    n: usize,
    mut parameter_at: impl FnMut(usize) -> mlua::Result<f64>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let input = read(lua, t, n)?;
    let mut out = buffer(n)?;
    for i in 0..n {
        if i.is_multiple_of(64) {
            check()?;
        }
        push(&mut out, kind.at(&input.xy, parameter_at(i)?)?);
    }
    check()?;
    columns(lua, &out)
}
pub(super) fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    g.set(
        "spline_point",
        lua.create_function(|lua, (t, u, kind): (Table, Value, Value)| {
            let kind = Kind::read(kind)?;
            let u = parameter(u)?;
            let input = read(lua, &t, 0)?;
            kind.at(&input.xy, u)
        })?,
    )?;
    g.set(
        "spline_points",
        lua.create_function(|lua, (t, parameters, kind): (Table, Table, Value)| {
            let kind = Kind::read(kind)?;
            let n = parameters.raw_len();
            if n == 0 || n > MAX_POINTS {
                return Err(stop(
                    Category::Budget,
                    "Spline evaluation needs1–4096 parameters.",
                ));
            }
            array(lua, &parameters, n)?;
            evaluate_controlled(
                lua,
                &t,
                kind,
                n,
                |i| parameter(parameters.raw_get(i + 1)?),
                || crate::lua::notes::check_budget(lua),
            )
        })?,
    )?;
    g.set(
        "sample_spline",
        lua.create_function(|lua, (t, options): (Table, Table)| {
            keys(&options, &["kind", "steps"])?;
            let kind = Kind::read(options.raw_get("kind")?)?;
            let n = steps(options.raw_get("steps")?, Some(32))?;
            evaluate_controlled(
                lua,
                &t,
                kind,
                n + 1,
                |i| Ok(i as f64 / n as f64),
                || crate::lua::notes::check_budget(lua),
            )
        })?,
    )
}

#[cfg(test)]
#[path = "geometry_spline_tests.rs"]
mod tests;
