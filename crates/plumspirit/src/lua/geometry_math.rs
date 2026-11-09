//! Stateless native random primitives and the existing math-only expression parser.
use super::*;
use plumspirit_engine::{
    editor::expr::{self, Expr},
    transform::rng::{Distribution, Rng},
};

const MAX_FORMULA_WORK: usize = 4_194_304;
// Includes AST boxes/vector capacity, bounded source/token strings and parser scratch.
const FORMULA_BYTES: usize = expr::MAX_NODES * 512 + expr::MAX_EXPR_LEN * 8 + 8192;

fn reserve(lua: &Lua, bytes: usize) -> mlua::Result<Reservation> {
    lua.app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("Mathematics accounting unavailable."))?
        .reserve(bytes)
        .map_err(|e| e.lua())
}
fn input_len(lua: &Lua, values: &Table) -> mlua::Result<usize> {
    let n = values.raw_len();
    if n == 0 || n > MAX_POINTS {
        return Err(stop(
            Category::Budget,
            "Mathematics needs1–4096 input values.",
        ));
    }
    array(lua, values, n)?;
    Ok(n)
}
fn word(v: Value, label: &str) -> mlua::Result<u64> {
    match v {
        Value::Integer(i) => Ok(i as u64),
        _ => Err(bad(&format!("{label} must be an exact Lua integer."))),
    }
}
fn index(v: Value) -> mlua::Result<u64> {
    match v {
        Value::Integer(i) if i >= 0 => Ok(i as u64),
        _ => Err(bad(
            "Random indices must be exact nonnegative Lua integers.",
        )),
    }
}
fn distribution(v: Value) -> mlua::Result<Distribution> {
    if matches!(v, Value::Nil) {
        return Ok(Distribution::Uniform);
    }
    let Value::String(s) = v else {
        return Err(bad("Random distribution must be bounded text."));
    };
    if s.as_bytes().len() > 32 {
        return Err(bad("Random distribution must be bounded text."));
    }
    match s.to_str()?.as_ref() {
        "uniform" => Ok(Distribution::Uniform),
        "gaussian" => Ok(Distribution::Gaussian),
        "triangular" => Ok(Distribution::Triangular),
        "exponential" => Ok(Distribution::Exponential),
        _ => Err(bad(
            "Random distribution must be uniform, gaussian, triangular or exponential.",
        )),
    }
}
fn random_controlled(
    lua: &Lua,
    seed: Value,
    indices: &Table,
    options: Option<&Table>,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    let seed = word(seed, "Random seed")?;
    let (salt, distribution) = if let Some(t) = options {
        keys(t, &["salt", "distribution"])?;
        let salt = match t.raw_get::<Value>("salt")? {
            Value::Nil => 0,
            v => word(v, "Random salt")?,
        };
        (salt, distribution(t.raw_get("distribution")?)?)
    } else {
        (0, Distribution::Uniform)
    };
    let n = input_len(lua, indices)?;
    let _workspace = reserve(lua, n * std::mem::size_of::<f64>() + 1024)?;
    let mut out = Vec::new();
    out.try_reserve_exact(n)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded random values."))?;
    for i in 1..=n {
        if (i - 1).is_multiple_of(64) {
            check()?;
        }
        out.push(Rng::for_index(seed, salt, index(indices.raw_get(i)?)?).bipolar(distribution));
    }
    check()?;
    let values = lua.create_table_with_capacity(n, 0)?;
    for (i, value) in out.into_iter().enumerate() {
        if i.is_multiple_of(64) {
            check()?;
        }
        values.raw_set(i + 1, value)?;
    }
    Ok(values)
}
fn formula(v: Value, axis: &str) -> mlua::Result<Expr> {
    let Value::String(s) = v else {
        return Err(bad("Formula x/y must be UTF-8 expression strings."));
    };
    if s.as_bytes().len() > expr::MAX_EXPR_LEN * 4 {
        return Err(stop(Category::Budget, "Formula exceeds4000UTF-8 bytes."));
    }
    Expr::parse(&s.to_str()?).map_err(|e| bad(&format!("{axis} formula: {e}")))
}
fn formula_controlled(
    lua: &Lua,
    parameters: &Table,
    options: &Table,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Table> {
    check()?;
    keys(options, &["x", "y"])?;
    let n = input_len(lua, parameters)?;
    let _workspace = reserve(lua, 2 * FORMULA_BYTES + n * std::mem::size_of::<Pt>())?;
    let x = formula(options.raw_get("x")?, "X")?;
    check()?;
    let y = formula(options.raw_get("y")?, "Y")?;
    if (x.cost() + y.cost()) * n > MAX_FORMULA_WORK {
        return Err(stop(Category::Budget, "Formula work exceeds4194304nodes."));
    }
    let mut out = Vec::new();
    out.try_reserve_exact(n)
        .map_err(|_| stop(Category::Memory, "Cannot allocate bounded formula output."))?;
    for i in 1..=n {
        if (i - 1).is_multiple_of(64) {
            check()?;
        }
        let t = coordinate(parameters.raw_get(i)?)?;
        let eval = |e: &Expr, axis: &str| {
            e.eval(t)
                .map_err(|e| bad(&format!("{axis} formula at sample{i}: {e}")))
                .and_then(|v| coordinate(Value::Number(v)))
        };
        out.push(Pt {
            x: eval(&x, "X")?,
            y: eval(&y, "Y")?,
            s: 0.,
        });
    }
    check()?;
    columns(lua, &out)
}
pub(super) fn install(lua: &Lua, g: &Table) -> mlua::Result<()> {
    g.set("max_formula_work", MAX_FORMULA_WORK)?;
    g.set(
        "random_values",
        lua.create_function(
            |lua, (seed, indices, options): (Value, Table, Option<Table>)| {
                random_controlled(lua, seed, &indices, options.as_ref(), || {
                    crate::lua::notes::check_budget(lua)
                })
            },
        )?,
    )?;
    g.set(
        "formula_points",
        lua.create_function(|lua, (parameters, options): (Table, Table)| {
            formula_controlled(lua, &parameters, &options, || {
                crate::lua::notes::check_budget(lua)
            })
        })?,
    )
}

#[cfg(test)]
#[path = "geometry_math_tests.rs"]
mod tests;
