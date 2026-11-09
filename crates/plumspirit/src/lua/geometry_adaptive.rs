//! Tolerance-aware native cubic flattening; fixed sampling APIs are unchanged.
use super::*;
use draw::geometry::{self as native, FlattenError};

fn limit(v: Value, default: usize, lo: usize, hi: usize) -> mlua::Result<usize> {
    match v {
        Value::Nil => Ok(default),
        Value::Integer(n) if n >= lo as i64 && n <= hi as i64 => Ok(n as usize),
        _ => Err(stop(Category::Budget, "Invalid adaptive flattening limit.")),
    }
}
struct Options {
    tolerance: f64,
    max_points: usize,
    max_depth: usize,
}
impl Options {
    fn read(opts: &Table, minimum: usize) -> mlua::Result<Self> {
        keys(opts, &["tolerance", "max_points", "max_depth"])?;
        let tolerance = coordinate(opts.raw_get("tolerance")?)?;
        if tolerance <= 0.0 {
            return Err(bad(
                "Flattening tolerance must be finite and positive, at most1e12.",
            ));
        }
        Ok(Self {
            tolerance,
            max_points: limit(opts.raw_get("max_points")?, MAX_POINTS, minimum, MAX_POINTS)?,
            max_depth: limit(
                opts.raw_get("max_depth")?,
                native::MAX_FLATTEN_DEPTH,
                0,
                native::MAX_FLATTEN_DEPTH,
            )?,
        })
    }
}
fn native_error(e: FlattenError<mlua::Error>) -> mlua::Error {
    match e {
        FlattenError::Stopped(e) => e,
        FlattenError::Invalid => {
            bad("Flattening tolerance must be finite and positive, at most1e12.")
        }
        FlattenError::Precision => bad(
            "Flattening tolerance is below coordinate precision; use local offsets or a larger tolerance.",
        ),
        FlattenError::Depth => stop(Category::Budget, "Flattening tolerance unmet at max_depth."),
        FlattenError::Points => stop(
            Category::Budget,
            "Flattening tolerance unmet at max_points.",
        ),
        FlattenError::Allocation => stop(
            Category::Memory,
            "Cannot allocate bounded flattening output.",
        ),
    }
}
fn flatten_controlled(
    lua: &Lua,
    control_points: &Table,
    opts: &Table,
    check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    let Options {
        tolerance,
        max_points,
        max_depth,
    } = Options::read(opts, 2)?;
    let (c, _input) = control(lua, control_points, 0)?;
    let _output = reserve(
        lua,
        max_points * std::mem::size_of::<Pt>() + native::FLATTEN_WORKSPACE_BYTES,
    )?;
    let p = native::flatten_cubic(c.p, tolerance, max_points, max_depth, check)
        .map_err(native_error)?;
    columns(lua, &p)
}
fn flatten_path_controlled(
    lua: &Lua,
    path: &Table,
    opts: &Table,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let Options {
        tolerance,
        max_points,
        max_depth,
    } = Options::read(opts, 1)?;
    // One endpoint per segment plus initial point and conditional closure is the
    // minimum possible output. The exact same parser serves fixed sampling.
    let ParsedPath {
        start,
        segments,
        close,
        count,
        _input,
    } = read_path(lua, path, 1, max_points)?;
    let curved = segments.iter().any(|s| matches!(s, Segment::Cubic(..)));
    let capacity = if curved { max_points } else { count };
    // A final path and one native cubic buffer may coexist. Reuse the existing
    // kernel unchanged; no intermediate Lua tables or retained native handles.
    let scratch = if curved {
        max_points * std::mem::size_of::<Pt>() + native::FLATTEN_WORKSPACE_BYTES
    } else {
        0
    };
    let _workspace = reserve(lua, capacity * std::mem::size_of::<Pt>() + scratch + 1024)?;
    let mut out = buffer(capacity)?;
    push(&mut out, start);
    let mut current = start;
    for (i, segment) in segments.iter().enumerate() {
        check()?;
        match *segment {
            Segment::Line(end) => {
                push(&mut out, end);
                current = end;
            }
            Segment::Cubic(a, b, end) => {
                // Keep one slot for each remaining segment and for closure.
                // The standalone cubic includes its shared starting point.
                let tail = segments.len() - i - 1 + usize::from(close);
                let available = max_points - out.len() - tail + 1;
                let sampled = native::flatten_cubic(
                    [current, a, b, end],
                    tolerance,
                    available,
                    max_depth,
                    &mut check,
                )
                .map_err(native_error)?;
                for (j, p) in sampled.iter().skip(1).enumerate() {
                    if j.is_multiple_of(64) {
                        check()?;
                    }
                    out.push(*p);
                }
                current = end;
            }
        }
    }
    if close {
        push(&mut out, start);
    }
    check()?;
    columns(lua, &out)
}
pub(super) fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    g.set(
        "flatten_path",
        lua.create_function(|lua, (path, opts): (Table, Table)| {
            flatten_path_controlled(lua, &path, &opts, || crate::lua::notes::check_budget(lua))
        })?,
    )?;
    g.set("max_flatten_depth", native::MAX_FLATTEN_DEPTH)?;
    g.set(
        "flatten_cubic",
        lua.create_function(|lua, (c, opts): (Table, Table)| {
            flatten_controlled(lua, &c, &opts, || crate::lua::notes::check_budget(lua))
        })?,
    )
}

#[cfg(test)]
#[path = "geometry_adaptive_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "geometry_adaptive_path_tests.rs"]
mod path_tests;
