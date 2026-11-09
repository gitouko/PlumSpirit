//! Boolean note-cell sets over the existing canonical row-span representation.
use super::*;
#[derive(Clone, Copy)]
enum Op {
    Union,
    Intersection,
    Difference,
    Xor,
}
impl Op {
    fn read(value: Value) -> mlua::Result<Self> {
        match value {
            Value::String(s) if s.as_bytes().len() <= 32 => match s.to_str()?.as_ref() {
                "union" => Ok(Self::Union),
                "intersection" => Ok(Self::Intersection),
                "difference" => Ok(Self::Difference),
                "xor" => Ok(Self::Xor),
                _ => Err(bad(
                    "Raster operation must be union, intersection, difference or xor.",
                )),
            },
            _ => Err(bad("Raster operation must be bounded text.")),
        }
    }
    fn inside(self, a: bool, b: bool) -> bool {
        match self {
            Self::Union => a || b,
            Self::Intersection => a && b,
            Self::Difference => a && !b,
            Self::Xor => a != b,
        }
    }
}
struct Input {
    row: Table,
    first: Table,
    last: Table,
    count: usize,
    cells: Option<u64>,
}
impl Input {
    fn read(lua: &Lua, t: &Table) -> mlua::Result<Self> {
        keys(t, &["row", "first", "last", "count", "cell_count"])?;
        let row: Table = t.raw_get("row")?;
        let first: Table = t.raw_get("first")?;
        let last: Table = t.raw_get("last")?;
        let count = row.raw_len();
        if count > MAX_SPANS || first.raw_len() != count || last.raw_len() != count {
            return Err(stop(
                Category::Budget,
                "Raster input columns must have equal lengths of at most65536.",
            ));
        }
        for column in [&row, &first, &last] {
            array(lua, column, count)?;
        }
        match t.raw_get::<Value>("count")? {
            Value::Nil => {}
            Value::Integer(n) if n >= 0 && n as usize == count => {}
            _ => return Err(bad("Raster count must be an integer matching its columns.")),
        }
        let cells = match t.raw_get::<Value>("cell_count")? {
            Value::Nil => None,
            Value::Integer(n) if n >= 0 => Some(n as u64),
            _ => return Err(bad("Raster cell_count must be a nonnegative integer.")),
        };
        Ok(Self {
            row,
            first,
            last,
            count,
            cells,
        })
    }
    fn spans(
        &self,
        grid: &Grid,
        check: &mut impl FnMut() -> mlua::Result<()>,
    ) -> mlua::Result<Vec<Span>> {
        let mut spans = Vec::new();
        spans
            .try_reserve_exact(self.count)
            .map_err(|_| stop(Category::Memory, "Cannot allocate raster Boolean input."))?;
        for i in 1..=self.count {
            if i.is_multiple_of(64) {
                check()?;
            }
            let index = |value: Value, max: usize| match value {
                Value::Integer(n) if n >= 0 && n as u64 <= max as u64 => Ok(n as u32),
                _ => Err(bad(
                    "Raster span coordinates must be integers inside the supplied grid.",
                )),
            };
            let row = index(self.row.raw_get(i)?, grid.rows - 1)?;
            let first = index(self.first.raw_get(i)?, grid.columns - 1)?;
            let last = index(self.last.raw_get(i)?, grid.columns)?;
            if first >= last {
                return Err(bad(
                    "Raster spans must have positive length (first < last).",
                ));
            }
            spans.push(Span { row, first, last });
        }
        normalize_spans(&mut spans, &mut *check)?;
        let mut cells = 0u64;
        for (i, s) in spans.iter().enumerate() {
            if i.is_multiple_of(64) {
                check()?;
            }
            cells += u64::from(s.last - s.first);
        }
        if self.cells.is_some_and(|expected| expected != cells) {
            return Err(bad("Raster cell_count must match canonical cell coverage."));
        }
        Ok(spans)
    }
}
/// Reuse the Boolean raster's strict input and canonical union, retaining the
/// native scratch reservation for as long as the caller uses the span buffer.
pub(in crate::lua::geometry) fn mask_spans(
    lua: &Lua,
    input: &Table,
    width: u32,
    height: u32,
    check: &mut impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<(Vec<Span>, Reservation)> {
    check()?;
    let input = Input::read(lua, input)?;
    let scratch = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Geometry accounting unavailable."))?
        .reserve(input.count * std::mem::size_of::<Span>() + 1024)
        .map_err(|e| e.lua())?;
    let grid = Grid {
        origin: (0., 0.),
        cell: (1., 1.),
        columns: width as usize,
        rows: height as usize,
        cap: MAX_SPANS,
    };
    Ok((input.spans(&grid, check)?, scratch))
}
fn emit(out: &mut Vec<Span>, span: Span, cap: usize) -> mlua::Result<()> {
    if span.first == span.last {
        return Ok(());
    }
    if let Some(last) = out.last_mut()
        && last.row == span.row
        && last.last == span.first
    {
        last.last = span.last;
        return Ok(());
    }
    if out.len() == cap {
        return Err(stop(
            Category::Budget,
            "Raster Boolean result exceeds max_spans after coalescing; split or simplify the inputs.",
        ));
    }
    out.push(span);
    Ok(())
}
fn sweep(
    a: &[Span],
    b: &[Span],
    op: Op,
    out: &mut Vec<Span>,
    cap: usize,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<()> {
    let (mut ia, mut ib, mut events) = (0usize, 0usize, 0usize);
    check()?;
    while ia < a.len() || ib < b.len() {
        let row = a
            .get(ia)
            .map_or(u32::MAX, |s| s.row)
            .min(b.get(ib).map_or(u32::MAX, |s| s.row));
        let (mut inside_a, mut inside_b, mut previous) = (false, false, 0u32);
        loop {
            if events.is_multiple_of(64) {
                check()?;
            }
            events += 1;
            let ax = a
                .get(ia)
                .filter(|s| s.row == row)
                .map_or(u32::MAX, |s| if inside_a { s.last } else { s.first });
            let bx = b
                .get(ib)
                .filter(|s| s.row == row)
                .map_or(u32::MAX, |s| if inside_b { s.last } else { s.first });
            let x = ax.min(bx);
            if x == u32::MAX {
                break;
            }
            if previous < x && op.inside(inside_a, inside_b) {
                emit(
                    out,
                    Span {
                        row,
                        first: previous,
                        last: x,
                    },
                    cap,
                )?;
            }
            if x == ax {
                if inside_a {
                    ia += 1;
                }
                inside_a = !inside_a;
            }
            if x == bx {
                if inside_b {
                    ib += 1;
                }
                inside_b = !inside_b;
            }
            previous = x;
        }
    }
    check()
}
fn controlled(
    lua: &Lua,
    a: Table,
    b: Table,
    grid: Table,
    operation: Value,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let grid = Grid::read(&grid)?;
    let op = Op::read(operation)?;
    let a = Input::read(lua, &a)?;
    let b = Input::read(lua, &b)?;
    // No operation can produce more than a.count+b.count canonical spans.
    let cap = grid.cap.min(a.count + b.count);
    let _native = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Geometry accounting unavailable."))?
        .reserve((a.count + b.count + cap) * std::mem::size_of::<Span>() + 1024)
        .map_err(|e| e.lua())?;
    let a = a.spans(&grid, &mut check)?;
    let b = b.spans(&grid, &mut check)?;
    let mut out = Vec::new();
    out.try_reserve_exact(cap)
        .map_err(|_| stop(Category::Memory, "Cannot allocate raster Boolean result."))?;
    sweep(&a, &b, op, &mut out, cap, &mut check)?;
    output(lua, &out, check)
}
pub(super) fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    g.set("max_raster_boolean_input_spans", MAX_SPANS * 2)?;
    g.set(
        "raster_boolean",
        lua.create_function(|lua, (a, b, grid, op): (Table, Table, Table, Value)| {
            controlled(lua, a, b, grid, op, || crate::lua::notes::check_budget(lua))
        })?,
    )
}
#[cfg(test)]
#[path = "geometry_raster_boolean_tests.rs"]
mod tests;
