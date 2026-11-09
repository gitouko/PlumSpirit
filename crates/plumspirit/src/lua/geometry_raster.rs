//! Bounded native pen rasterization into unique grid-row spans, not MIDI edits.
use super::*;
use plumspirit_engine::transform::generator::{
    area::{self, Contour, Figure, Geom, GeomSpec, RowBuf, RowState},
    pen::{Band, Brush, Draw, Pen, Sink},
};

#[path = "geometry_raster_boolean.rs"]
mod boolean;
#[path = "geometry_raster_stroke.rs"]
mod stroke;

const MAX_COLUMNS: usize = 65536;
const MAX_ROWS: usize = 4096;
const MAX_SPANS: usize = 65536;
const MAX_WORK: usize = 4 * 1024 * 1024;
type Span = RasterSpan;
pub(super) use boolean::mask_spans;
struct Grid {
    origin: (f64, f64),
    cell: (f64, f64),
    columns: usize,
    rows: usize,
    cap: usize,
}
fn integer(v: Value, default: Option<usize>, max: usize) -> mlua::Result<usize> {
    match (v, default) {
        (Value::Nil, Some(n)) => Ok(n),
        (Value::Integer(n), _) if n >= 1 && n <= max as i64 => Ok(n as usize),
        _ => Err(stop(
            Category::Budget,
            "Invalid raster grid dimension or span limit.",
        )),
    }
}
fn number(t: &Table, key: &str, default: f64) -> mlua::Result<f64> {
    match t.raw_get::<Value>(key)? {
        Value::Nil => Ok(default),
        v => coordinate(v),
    }
}
impl Grid {
    fn read(t: &Table) -> mlua::Result<Self> {
        keys(
            t,
            &[
                "origin_x",
                "origin_y",
                "cell_width",
                "cell_height",
                "columns",
                "rows",
                "max_spans",
            ],
        )?;
        let cell = (number(t, "cell_width", 1.)?, number(t, "cell_height", 1.)?);
        if cell.0 <= 0. || cell.1 <= 0. {
            return Err(bad("Raster cell dimensions must be finite and positive."));
        }
        Ok(Self {
            origin: (number(t, "origin_x", 0.)?, number(t, "origin_y", 0.)?),
            cell,
            columns: integer(t.raw_get("columns")?, None, MAX_COLUMNS)?,
            rows: integer(t.raw_get("rows")?, None, MAX_ROWS)?,
            cap: integer(t.raw_get("max_spans")?, Some(8192), MAX_SPANS)?,
        })
    }
}
struct Spans {
    points: Vec<Span>,
    columns: usize,
    rows: usize,
    cap: usize,
}
impl Sink for Spans {
    fn note(&mut self, start: i128, dur: i128, row: i64, _: &Brush) -> bool {
        if row < 0 || row >= self.rows as i64 {
            return true;
        }
        let first = start.clamp(0, self.columns as i128) as u32;
        let last = start.saturating_add(dur).clamp(0, self.columns as i128) as u32;
        if first >= last {
            return true;
        }
        if self.points.len() == self.cap {
            return false;
        }
        self.points.push(Span {
            row: row as u32,
            first,
            last,
        });
        true
    }
}
fn output(
    lua: &Lua,
    p: &[Span],
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    let (rows, first, last) = (
        lua.create_table_with_capacity(p.len(), 0)?,
        lua.create_table_with_capacity(p.len(), 0)?,
        lua.create_table_with_capacity(p.len(), 0)?,
    );
    let mut cells = 0u64;
    for (i, p) in p.iter().enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        rows.raw_set(i + 1, p.row)?;
        first.raw_set(i + 1, p.first)?;
        last.raw_set(i + 1, p.last)?;
        cells += u64::from(p.last - p.first);
    }
    check()?;
    out.raw_set("count", p.len())?;
    out.raw_set("cell_count", cells)?;
    out.raw_set("row", rows)?;
    out.raw_set("first", first)?;
    out.raw_set("last", last)?;
    Ok(out)
}
fn raster_controlled(
    lua: &Lua,
    t: &Table,
    grid: &Table,
    fill: bool,
    closed: bool,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let grid = Grid::read(grid)?;
    let (source, _input) = points(lua, t, 0)?;
    if fill && source.len() < 3 {
        return Err(bad("Polygon rasterization needs at least three points."));
    }
    let extra = usize::from(
        !fill
            && closed
            && source
                .first()
                .zip(source.last())
                .is_some_and(|(a, b)| a.x != b.x || a.y != b.y),
    );
    let vertices = source.len() + extra;
    if vertices * grid.rows > MAX_WORK {
        return Err(stop(
            Category::Budget,
            "Raster work exceeds4194304 vertex-row tests; crop the grid or split the shape.",
        ));
    }
    let _workspace = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Geometry accounting unavailable."))?
        .reserve(
            vertices * std::mem::size_of::<(f64, f64)>()
                + if fill {
                    // Native stable crossing sort may allocate temporary storage.
                    2 * vertices * std::mem::size_of::<f64>()
                } else {
                    0
                }
                + grid.cap * std::mem::size_of::<Span>()
                + 1024,
        )
        .map_err(|e| e.lua())?;
    let mut poly = Vec::new();
    poly.try_reserve_exact(vertices)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded raster input."))?;
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
                "Raster grid coordinates exceed finite +/-1e12; use local offsets or larger cells.",
            ));
        }
        poly.push((x, y));
    }
    if extra != 0 {
        poly.push(poly[0]);
    }
    let mut sink = Spans {
        points: Vec::new(),
        columns: grid.columns,
        rows: grid.rows,
        cap: grid.cap,
    };
    sink.points
        .try_reserve_exact(grid.cap)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded raster spans."))?;
    let mut pen = Pen::new(
        sink,
        1,
        Band {
            lo: 0,
            hi: grid.rows as i64,
        },
    );
    if fill {
        pen.reserve_fill(vertices).map_err(|_| {
            stop(
                Category::Memory,
                "Cannot allocate bounded raster crossings.",
            )
        })?;
    }
    let brush = Brush {
        vel: 1,
        gate: 1.,
        sub: 0,
    };
    let mut failure = None;
    let mut proceed = || match check() {
        Ok(()) => true,
        Err(e) => {
            failure = Some(e);
            false
        }
    };
    let ok = if fill {
        pen.fill_controlled(&poly, brush, &mut proceed)
    } else {
        pen.stroke_controlled(&poly, brush, &mut proceed)
    };
    if let Some(e) = failure {
        return Err(e);
    }
    if !ok {
        return Err(stop(
            Category::Budget,
            "Rasterization exceeds max_spans before union; reduce or split the shape.",
        ));
    }
    let mut spans = pen.finish().points;
    finish_spans(lua, &mut spans, check)
}
fn normalize_spans(
    spans: &mut Vec<Span>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<()> {
    check()?;
    spans.sort_unstable_by_key(|p| (p.row, p.first, p.last));
    check()?;
    let mut n = 0usize;
    for i in 0..spans.len() {
        if i.is_multiple_of(64) {
            check()?;
        }
        let p = spans[i];
        if n != 0 && spans[n - 1].row == p.row && p.first <= spans[n - 1].last {
            spans[n - 1].last = spans[n - 1].last.max(p.last);
        } else {
            spans[n] = p;
            n += 1;
        }
    }
    spans.truncate(n);
    Ok(())
}
fn finish_spans(
    lua: &Lua,
    spans: &mut Vec<Span>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    normalize_spans(spans, &mut check)?;
    output(lua, spans, check)
}
// Fill-only Geom never invokes boundary pair expansion or native stroke building.
// At <=4096 edges, each uncancellable build/row phase is independently bounded.
const MAX_CONTOURS: usize = 256;
fn region_controlled(
    lua: &Lua,
    contours: &Table,
    grid: &Table,
    opts: Option<Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let grid = Grid::read(grid)?;
    let rule = if let Some(opts) = opts {
        keys(&opts, &["rule"])?;
        match opts.raw_get::<Value>("rule")? {
            Value::Nil => area::Rule::EvenOdd,
            Value::String(s) if s.to_str()? == "even_odd" => area::Rule::EvenOdd,
            Value::String(s) if s.to_str()? == "non_zero" => area::Rule::NonZero,
            _ => return Err(bad("Region rule must be even_odd or non_zero.")),
        }
    } else {
        area::Rule::EvenOdd
    };
    let n = contours.raw_len();
    if n == 0 || n > MAX_CONTOURS {
        return Err(stop(Category::Budget, "Region needs1–256 contours."));
    }
    array(lua, contours, n)?;
    let budget = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Geometry accounting unavailable."))?;
    let _descriptors = budget.reserve(n * 256 + 1024).map_err(|e| e.lua())?;
    let mut total = 0usize;
    for i in 1..=n {
        check()?;
        let t = contours.raw_get::<Table>(i)?;
        keys(&t, &["x", "y", "count"])?;
        let count = t.raw_get::<Table>("x")?.raw_len();
        if !(3..=MAX_POINTS).contains(&count) || total + count > MAX_POINTS {
            return Err(stop(
                Category::Budget,
                "Region needs3points per contour and at most4096 total vertices.",
            ));
        }
        total += count;
    }
    if total * grid.rows > MAX_WORK {
        return Err(stop(
            Category::Budget,
            "Region raster work exceeds4194304 vertex-row tests; crop the grid or split the shape.",
        ));
    }
    // Covers simultaneously retained contours (points/tags/chords), build Src/Seg/
    // ordering+stable-sort scratch and all row buffers/active tables. Native Vec
    // doubling is included; no boundary/band/stroke buffers are constructed.
    let _workspace = budget
        .reserve(total * 1024 + grid.cap * std::mem::size_of::<Span>() + 16384)
        .map_err(|e| e.lua())?;
    let mut cs = Vec::with_capacity(n);
    for i in 1..=n {
        check()?;
        let (p, _input) = points(lua, &contours.raw_get::<Table>(i)?, 0)?;
        let mut xy = Vec::with_capacity(p.len());
        for (i, p) in p.iter().enumerate() {
            if i.is_multiple_of(64) {
                check()?;
            }
            let (x, y) = (
                (p.x - grid.origin.0) / grid.cell.0,
                (p.y - grid.origin.1) / grid.cell.1,
            );
            if !x.is_finite() || !y.is_finite() || x.abs() > 1e12 || y.abs() > 1e12 {
                return Err(bad(
                    "Raster grid coordinates exceed finite +/-1e12; use local offsets or larger cells.",
                ));
            }
            xy.push((x, y));
        }
        cs.push(Contour::polygon(xy, 0, true));
    }
    check()?;
    let spec = GeomSpec {
        area: vec![Figure {
            layers: vec![(cs, rule)],
            ..Default::default()
        }],
        ..Default::default()
    };
    let geom = Geom::build(&spec).map_err(|e| bad(&e))?;
    check()?;
    scan_geom(lua, &geom, &grid, false, check)
}
fn scan_geom(
    lua: &Lua,
    geom: &Geom,
    grid: &Grid,
    stroke: bool,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    let mut sink = Spans {
        points: Vec::new(),
        columns: grid.columns,
        rows: grid.rows,
        cap: grid.cap,
    };
    sink.points
        .try_reserve_exact(grid.cap)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded raster spans."))?;
    let mut pen = Pen::new(
        sink,
        1,
        Band {
            lo: 0,
            hi: grid.rows as i64,
        },
    );
    let (mut state, mut buffers, mut edge, mut interior) = (
        RowState::default(),
        RowBuf::default(),
        Vec::new(),
        Vec::new(),
    );
    let brush = Brush {
        vel: 1,
        gate: 1.,
        sub: 0,
    };
    for row in 0..grid.rows {
        check()?;
        geom.row(
            row as f64 + 0.5,
            &mut state,
            &mut buffers,
            &mut edge,
            &mut interior,
        );
        let intervals = if stroke { &edge } else { &interior };
        for (i, &(a, b)) in intervals.iter().enumerate() {
            if i.is_multiple_of(64) {
                check()?;
            }
            if !a.is_finite() || !b.is_finite() {
                return Err(bad(if stroke {
                    "Native stroke produced a nonfinite crossing."
                } else {
                    "Native region produced a nonfinite crossing."
                }));
            }
            if !pen.note(a, b, row as i64, brush) {
                return Err(stop(
                    Category::Budget,
                    if stroke {
                        "Stroke rasterization exceeds max_spans before union; reduce or split the shape."
                    } else {
                        "Region rasterization exceeds max_spans before union; reduce or split the shape."
                    },
                ));
            }
        }
    }
    let mut spans = pen.finish().points;
    finish_spans(lua, &mut spans, check)
}

pub(super) fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    boolean::install(lua, g)?;
    stroke::install(lua, g)?;
    for (name, value) in [
        ("max_raster_columns", MAX_COLUMNS),
        ("max_raster_rows", MAX_ROWS),
        ("max_raster_spans", MAX_SPANS),
        ("max_raster_work", MAX_WORK),
        ("max_region_contours", MAX_CONTOURS),
    ] {
        g.set(name, value)?;
    }
    g.set(
        "rasterize_region",
        lua.create_function(
            |lua, (contours, grid, opts): (Table, Table, Option<Table>)| {
                region_controlled(lua, &contours, &grid, opts, || {
                    crate::lua::notes::check_budget(lua)
                })
            },
        )?,
    )?;
    g.set(
        "rasterize_polygon",
        lua.create_function(|lua, (t, grid): (Table, Table)| {
            raster_controlled(lua, &t, &grid, true, false, || {
                crate::lua::notes::check_budget(lua)
            })
        })?,
    )?;
    g.set(
        "rasterize_polyline",
        lua.create_function(|lua, (t, grid, opts): (Table, Table, Option<Table>)| {
            let closed = if let Some(opts) = opts {
                keys(&opts, &["closed"])?;
                match opts.raw_get::<Value>("closed")? {
                    Value::Nil => false,
                    Value::Boolean(v) => v,
                    _ => return Err(bad("Raster closed must be a boolean.")),
                }
            } else {
                false
            };
            raster_controlled(lua, &t, &grid, false, closed, || {
                crate::lua::notes::check_budget(lua)
            })
        })?,
    )
}

#[cfg(test)]
#[path = "geometry_raster_tests.rs"]
mod tests;
