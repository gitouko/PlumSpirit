//! Thick paths using the existing native stroke parts and row scanner.
use super::*;
use area::{Cap, Join};
struct Options {
    half: f64,
    closed: bool,
    join: Join,
    cap: Cap,
}
impl Options {
    fn read(t: &Table) -> mlua::Result<Self> {
        keys(t, &["width_cells", "closed", "join", "cap"])?;
        let width = coordinate(t.raw_get("width_cells")?)?;
        if width <= 0. || width / 2. == 0. {
            return Err(bad(
                "Stroke width_cells must be finite, positive and representable.",
            ));
        }
        let closed = match t.raw_get::<Value>("closed")? {
            Value::Nil => false,
            Value::Boolean(v) => v,
            _ => return Err(bad("Stroke closed must be a boolean.")),
        };
        let word = |key: &str, default: &str| -> mlua::Result<String> {
            match t.raw_get::<Value>(key)? {
                Value::Nil => Ok(default.into()),
                Value::String(s) if s.as_bytes().len() <= 32 => Ok(s.to_str()?.to_string()),
                _ => Err(bad("Stroke cap/join must be bounded text.")),
            }
        };
        let join = match word("join", "round")?.as_str() {
            "round" => Join::Round,
            "miter" => Join::Mitre,
            "bevel" => Join::Bevel,
            _ => return Err(bad("Stroke join must be round, miter or bevel.")),
        };
        let cap = match word("cap", "round")?.as_str() {
            "round" => Cap::Round,
            "butt" => Cap::Butt,
            "square" => Cap::Square,
            _ => return Err(bad("Stroke cap must be round, butt or square.")),
        };
        Ok(Self {
            half: width / 2.,
            closed,
            join,
            cap,
        })
    }
}
fn controlled(
    lua: &Lua,
    points_table: &Table,
    grid_table: &Table,
    options: &Table,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let grid = Grid::read(grid_table)?;
    let options = Options::read(options)?;
    let (source, _input) = points(lua, points_table, 0)?;
    let parts = source.len() * 2 + 2;
    if parts * grid.rows > MAX_WORK {
        return Err(stop(
            Category::Budget,
            "Stroke raster work exceeds4194304 conservative part-row tests; crop the grid or simplify the path.",
        ));
    }
    // Covers normalized/tagged/chord inputs, native stroke parts/edges/order and
    // overlapping capacity growth, row active/union/sort buffers plus output.
    let _workspace = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Geometry accounting unavailable."))?
        .reserve(source.len() * 2048 + grid.cap * std::mem::size_of::<Span>() + 32768)
        .map_err(|e| e.lua())?;
    let mut xy = Vec::new();
    xy.try_reserve_exact(source.len())
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded stroke points."))?;
    let mut magnitude = 1f64;
    for (i, p) in source.iter().enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        let (x, y) = (
            (p.x - grid.origin.0) / grid.cell.0,
            (p.y - grid.origin.1) / grid.cell.1,
        );
        if !x.is_finite() || !y.is_finite() || x.abs() > 1e12 || y.abs() > 1e12 {
            return Err(bad(
                "Stroke grid coordinates exceed finite +/-1e12; use local offsets or larger cells.",
            ));
        }
        magnitude = magnitude.max(x.abs()).max(y.abs());
        xy.push((x, y));
    }
    if options.half < magnitude * f64::EPSILON * 8. {
        return Err(bad(
            "Stroke width is below representable precision in this grid; use local offsets or rescale the path.",
        ));
    }
    let spec = GeomSpec {
        strokes: vec![Contour::polygon(xy, 0, options.closed)],
        pen: (options.half, options.join, options.cap),
        ..Default::default()
    };
    check()?;
    let geom = Geom::build(&spec).map_err(|e| bad(&e))?;
    check()?;
    // Empty area/band ensures Geom never performs boundary pair expansion.
    scan_geom(lua, &geom, &grid, true, check)
}
pub(super) fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    g.set("stroke_miter_limit", area::MITRE_LIMIT)?;
    g.set(
        "rasterize_stroke",
        lua.create_function(|lua, (p, grid, options): (Table, Table, Table)| {
            controlled(lua, &p, &grid, &options, || {
                crate::lua::notes::check_budget(lua)
            })
        })?,
    )
}
#[cfg(test)]
#[path = "geometry_raster_stroke_tests.rs"]
mod tests;
