//! Pure bounded curve/path sampling. No Lua callbacks enter a native evaluator.
use super::*;

#[path = "geometry_adaptive.rs"]
mod adaptive;
#[path = "geometry_splines.rs"]
mod splines;

pub(super) const MAX_PATH_SEGMENTS: usize = 256;
type Point = (f64, f64);
#[derive(Clone, Copy)]
struct Cubic {
    p: [Point; 4],
    bounds: [f64; 4],
}
impl Cubic {
    fn new(p: [Point; 4]) -> Self {
        let bounds = p.iter().fold([p[0].0, p[0].1, p[0].0, p[0].1], |b, p| {
            [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)]
        });
        Self { p, bounds }
    }
    fn at(self, t: f64) -> Point {
        if t == 0.0 {
            return self.p[0];
        }
        if t == 1.0 {
            return self.p[3];
        }
        let (x, y) = draw::cubic(self.p[0], self.p[1], self.p[2], self.p[3], t);
        // A Bézier lies inside its control hull. Limit floating-point drift at
        // the input bounds so results remain composable with geometry columns.
        (
            x.clamp(self.bounds[0], self.bounds[2]),
            y.clamp(self.bounds[1], self.bounds[3]),
        )
    }
}
fn reserve(lua: &Lua, bytes: usize) -> mlua::Result<Reservation> {
    lua.app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Geometry accounting unavailable."))?
        .reserve(bytes)
        .map_err(|e| e.lua())
}
fn parameter(v: Value) -> mlua::Result<f64> {
    let t = match v {
        Value::Integer(n) => n as f64,
        Value::Number(n) => n,
        _ => return Err(bad("A curve parameter must be a number in [0,1].")),
    };
    if !t.is_finite() || !(0.0..=1.0).contains(&t) {
        return Err(bad("A curve parameter must be finite in [0,1]."));
    }
    Ok(t)
}
fn steps(v: Value, default: Option<usize>) -> mlua::Result<usize> {
    match (v, default) {
        (Value::Nil, Some(n)) => Ok(n),
        (Value::Integer(n), _) if n >= 1 && n < MAX_POINTS as i64 => Ok(n as usize),
        _ => Err(stop(
            Category::Budget,
            "Curve steps must be an integer1–4095.",
        )),
    }
}
fn control(lua: &Lua, t: &Table, output: usize) -> mlua::Result<(Cubic, Reservation)> {
    keys(t, &["x", "y", "count"])?;
    if t.raw_get::<Table>("x")?.raw_len() != 4 || t.raw_get::<Table>("y")?.raw_len() != 4 {
        return Err(bad(
            "A cubic needs exactly four control points in x/y columns.",
        ));
    }
    let (p, native) = points(lua, t, output)?;
    Ok((
        Cubic::new([
            (p[0].x, p[0].y),
            (p[1].x, p[1].y),
            (p[2].x, p[2].y),
            (p[3].x, p[3].y),
        ]),
        native,
    ))
}
fn buffer(count: usize) -> mlua::Result<Vec<Pt>> {
    let mut p = Vec::new();
    p.try_reserve_exact(count)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded curve output."))?;
    Ok(p)
}
fn push(p: &mut Vec<Pt>, (x, y): Point) {
    p.push(Pt { x, y, s: 0.0 });
}
fn append_cubic(
    p: &mut Vec<Pt>,
    c: Cubic,
    steps: usize,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<()> {
    for i in 1..=steps {
        if p.len().is_multiple_of(64) {
            check()?;
        }
        push(p, c.at(i as f64 / steps as f64));
    }
    check()
}
fn sample_uniform_controlled(
    lua: &Lua,
    t: &Table,
    steps: usize,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let (c, _native) = control(lua, t, steps + 1)?;
    let mut p = buffer(steps + 1)?;
    push(&mut p, c.p[0]);
    append_cubic(&mut p, c, steps, &mut check)?;
    columns(lua, &p)
}

#[derive(Clone, Copy)]
enum Segment {
    Line(Point),
    Cubic(Point, Point, Point),
}
fn named_point(t: &Table, x: &str, y: &str) -> mlua::Result<Point> {
    Ok((coordinate(t.raw_get(x)?)?, coordinate(t.raw_get(y)?)?))
}
struct ParsedPath {
    start: Point,
    segments: Vec<Segment>,
    close: bool,
    count: usize,
    _input: Reservation,
}
fn read_path(
    lua: &Lua,
    path: &Table,
    curve_steps: usize,
    max_points: usize,
) -> mlua::Result<ParsedPath> {
    super::super::notes::check_budget(lua)?;
    keys(path, &["start", "segments", "closed"])?;
    let start = path.raw_get::<Table>("start")?;
    keys(&start, &["x", "y"])?;
    let start = named_point(&start, "x", "y")?;
    let closed = match path.raw_get::<Value>("closed")? {
        Value::Nil => false,
        Value::Boolean(b) => b,
        _ => return Err(bad("Path closed must be a boolean.")),
    };
    let segments = path.raw_get::<Table>("segments")?;
    let n = segments.raw_len();
    if n > MAX_PATH_SEGMENTS {
        return Err(stop(Category::Budget, "A path allows at most256 segments."));
    }
    array(lua, &segments, n)?;
    let _input = reserve(lua, n * std::mem::size_of::<Segment>() + 1024)?;
    let mut parsed = Vec::new();
    parsed
        .try_reserve_exact(n)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded path input."))?;
    let (mut count, mut last) = (1usize, start);
    for i in 1..=n {
        if i % 64 == 0 {
            super::super::notes::check_budget(lua)?;
        }
        let t = segments.raw_get::<Table>(i)?;
        if t.metatable().is_some() {
            return Err(bad("Path segments must be plain tables."));
        }
        let kind = t.raw_get::<mlua::LuaString>("kind")?;
        let (segment, cost) = match kind.to_str()?.as_ref() {
            "line" => {
                keys(&t, &["kind", "x", "y"])?;
                let end = named_point(&t, "x", "y")?;
                last = end;
                (Segment::Line(end), 1)
            }
            "cubic" => {
                keys(&t, &["kind", "x1", "y1", "x2", "y2", "x", "y"])?;
                let a = named_point(&t, "x1", "y1")?;
                let b = named_point(&t, "x2", "y2")?;
                let end = named_point(&t, "x", "y")?;
                last = end;
                (Segment::Cubic(a, b, end), curve_steps)
            }
            _ => return Err(bad("A path segment kind must be line or cubic.")),
        };
        count = count
            .checked_add(cost)
            .filter(|&c| c <= max_points)
            .ok_or_else(|| {
                stop(
                    Category::Budget,
                    "Path output exceeds max_points (at most4096); split the path.",
                )
            })?;
        parsed.push(segment);
    }
    let close = closed && last != start;
    if close {
        count = count
            .checked_add(1)
            .filter(|&c| c <= max_points)
            .ok_or_else(|| {
                stop(
                    Category::Budget,
                    "Closing the path exceeds max_points (at most4096 output points).",
                )
            })?;
    }
    Ok(ParsedPath {
        start,
        segments: parsed,
        close,
        count,
        _input,
    })
}
fn sample_path(lua: &Lua, path: Table, opts: Option<Table>) -> mlua::Result<Table> {
    let curve_steps = if let Some(opts) = opts {
        keys(&opts, &["curve_steps"])?;
        steps(opts.raw_get("curve_steps")?, Some(32))?
    } else {
        32
    };
    let ParsedPath {
        start,
        segments: parsed,
        close,
        count,
        _input,
    } = read_path(lua, &path, curve_steps, MAX_POINTS)?;
    let _output = reserve(lua, count * std::mem::size_of::<Pt>() + 1024)?;
    let mut out = buffer(count)?;
    push(&mut out, start);
    let mut current = start;
    for (i, s) in parsed.into_iter().enumerate() {
        if i % 64 == 0 {
            super::super::notes::check_budget(lua)?;
        }
        match s {
            Segment::Line(end) => {
                push(&mut out, end);
                current = end;
            }
            Segment::Cubic(a, b, end) => {
                append_cubic(
                    &mut out,
                    Cubic::new([current, a, b, end]),
                    curve_steps,
                    || super::super::notes::check_budget(lua),
                )?;
                current = end;
            }
        }
    }
    if close {
        push(&mut out, start);
    }
    columns(lua, &out)
}
pub(super) fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    adaptive::install(lua, g)?;
    splines::install(lua, g)?;
    g.set("max_path_segments", MAX_PATH_SEGMENTS)?;
    g.set(
        "cubic_point",
        lua.create_function(|lua, (t, u): (Table, Value)| {
            super::super::notes::check_budget(lua)?;
            let u = parameter(u)?;
            let (c, _native) = control(lua, &t, 0)?;
            Ok(c.at(u))
        })?,
    )?;
    g.set(
        "cubic_points",
        lua.create_function(|lua, (t, parameters): (Table, Table)| {
            super::super::notes::check_budget(lua)?;
            let n = parameters.raw_len();
            if n == 0 || n > MAX_POINTS {
                return Err(stop(
                    Category::Budget,
                    "Cubic evaluation needs1–4096 parameters.",
                ));
            }
            array(lua, &parameters, n)?;
            let (c, _native) = control(lua, &t, n)?;
            let mut out = buffer(n)?;
            for i in 1..=n {
                if i % 64 == 0 {
                    super::super::notes::check_budget(lua)?;
                }
                push(&mut out, c.at(parameter(parameters.raw_get(i)?)?));
            }
            columns(lua, &out)
        })?,
    )?;
    g.set(
        "sample_cubic",
        lua.create_function(|lua, (t, n): (Table, Value)| {
            let n = steps(n, None)?;
            sample_uniform_controlled(lua, &t, n, || super::super::notes::check_budget(lua))
        })?,
    )?;
    g.set(
        "sample_path",
        lua.create_function(|lua, (path, opts): (Table, Option<Table>)| {
            sample_path(lua, path, opts)
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "geometry_curve_tests.rs"]
mod tests;
