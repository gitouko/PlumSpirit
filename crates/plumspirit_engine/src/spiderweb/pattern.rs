//! Formulas laid along a shape's path (`notes/pattern.py`): a **shape**
//! (a drawing x(t), y(t) fitted between the path's ends, then laid along it)
//! and a **pattern** (one loop y(x) repeated sideways along the path, maybe
//! moved along it too). Shape first, then pattern. Work is in screen units
//! x' = x / k (k: beats per key on screen when the formula was put on).

use super::P;
use super::curves::sample;
use super::formula::Formula;
use super::np::{dist, hypot, interp, linspace, np_hypot};
use super::{fnum, inum, pts_of, pts_value};
use crate::json::Value;

/// Points per loop (a multiple of 4, so a zigzag's tips are exact).
pub const LOOP_SAMPLES: usize = 256;
/// The most points a pattern makes.
pub const MAX_POINTS: i64 = 400_000;
/// Points along a shape formula.
pub const SHAPE_SAMPLES: usize = 1024;
pub const LOOPS_DEFAULT: f64 = 4.0;
/// A formula's symmetric halves.
pub const SYMS: [&str; 2] = ["mirror", "turn"];

/// (id, formula, its numbers) — the pattern presets.
/// A preset's numbers.
pub type Numbers = &'static [(&'static str, f64)];

pub const PATTERN_PRESETS: [(&str, &str, Numbers); 12] = [
    ("wave", "height * sin(x * 2 * pi)", &[("height", 4.0)]),
    (
        "zigzag",
        "height * 2 / pi * asin(sin(x * 2 * pi))",
        &[("height", 4.0)],
    ),
    ("bounce", "height * abs(sin(x * pi))", &[("height", 4.0)]),
    (
        "square",
        "height * max(-1, min(1, steep * sin(x * 2 * pi)))",
        &[("height", 4.0), ("steep", 8.0)],
    ),
    (
        "saw",
        "height * 2 * (x - floor(x + 0.5))",
        &[("height", 4.0)],
    ),
    ("teeth", "height * (1 - abs(2 * x - 1))", &[("height", 4.0)]),
    (
        "spikes",
        "height * (1 - abs(2 * x - 1))^sharp",
        &[("height", 4.0), ("sharp", 3.0)],
    ),
    (
        "bumps",
        "height * sqrt(max(0, 1 - (2 * x - 1)^2))",
        &[("height", 4.0)],
    ),
    (
        "stairs",
        "height * min(steps, floor((1 - abs(2 * x - 1)) * (steps + 1))) / steps",
        &[("height", 4.0), ("steps", 3.0)],
    ),
    (
        "swell",
        "height * sin(x * pi) * sin(x * waves * 2 * pi)",
        &[("height", 4.0), ("waves", 4.0)],
    ),
    (
        "fade",
        "height * (1 - x) * sin(x * waves * 2 * pi)",
        &[("height", 4.0), ("waves", 4.0)],
    ),
    (
        "curls",
        "height * (1 - cos(x * 2 * pi)) / 2",
        &[("height", 4.0), ("curl", 2.0)],
    ),
];
/// Presets that also move the line along the path (`along`).
pub const PRESET_ALONG: [(&str, &str); 1] = [("curls", "curl * sin(x * 2 * pi) / (2 * pi)")];

/// (id, x(t), y(t), its numbers) — the shape presets.
pub const SHAPE_PRESETS: [(&str, &str, &str, Numbers); 12] = [
    ("circle", "cos(t * 2 * pi)", "sin(t * 2 * pi)", &[]),
    (
        "spiral",
        "t * cos(t * turns * 2 * pi)",
        "t * sin(t * turns * 2 * pi)",
        &[("turns", 3.0)],
    ),
    (
        "flower",
        "(1 + depth * cos(petals * t * 2 * pi)) * cos(t * 2 * pi)",
        "(1 + depth * cos(petals * t * 2 * pi)) * sin(t * 2 * pi)",
        &[("petals", 5.0), ("depth", 0.3)],
    ),
    (
        "heart",
        "16 * sin(t * 2 * pi)^3",
        "13 * cos(t * 2 * pi) - 5 * cos(4 * pi * t) - 2 * cos(6 * pi * t) - cos(8 * pi * t)",
        &[],
    ),
    (
        "figure8",
        "sin(t * 2 * pi)",
        "sin(t * 2 * pi) * cos(t * 2 * pi)",
        &[],
    ),
    ("slow", "t", "t^bend", &[("bend", 2.0)]),
    ("fast", "t", "1 - (1 - t)^bend", &[("bend", 2.0)]),
    (
        "scurve",
        "t",
        "t^steep / (t^steep + (1 - t)^steep)",
        &[("steep", 2.0)],
    ),
    (
        "reverse_s",
        "t^steep / (t^steep + (1 - t)^steep)",
        "t",
        &[("steep", 2.0)],
    ),
    ("quarter", "sin(t * pi / 2)", "1 - cos(t * pi / 2)", &[]),
    (
        "exponential",
        "t",
        "(exp(steep * t) - 1) / (exp(steep) - 1)",
        &[("steep", 5.0)],
    ),
    (
        "logarithmic",
        "t",
        "ln(1 + steep * t) / ln(1 + steep)",
        &[("steep", 20.0)],
    ),
];

// ------------------------------------------------------------------ Python conversions

/// Python `str(x)` of a JSON value (as far as JSON keeps it).
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Num(n) if n.is_finite() && *n == n.trunc() && n.abs() < 1e16 => {
            format!("{}", *n as i64)
        }
        Value::Num(n) if n.is_nan() => "nan".into(),
        Value::Num(n) if n.is_infinite() => (if *n > 0.0 { "inf" } else { "-inf" }).into(),
        Value::Num(n) => format!("{n:?}"),
        other => other.to_text(),
    }
}

/// `float(d.get(key, default))` (None: Python raises).
fn get_float(d: &Value, key: &str, default: f64) -> Option<f64> {
    match d.get(key) {
        None => Some(default),
        Some(v) => fnum(v),
    }
}

/// `{str(a): float(b) for a, b in dict(d.get("vars", {})).items()}`; None
/// when Python raises. Later duplicates win.
fn vars_of(d: &Value) -> Option<Vec<(String, f64)>> {
    let mut out: Vec<(String, f64)> = Vec::new();
    let mut put = |k: String, v: f64| {
        if let Some(e) = out.iter_mut().find(|(n, _)| *n == k) {
            e.1 = v;
        } else {
            out.push((k, v));
        }
    };
    match d.get("vars") {
        None => {}
        Some(Value::Obj(o)) => {
            for (k, v) in o {
                put(k.clone(), fnum(v)?);
            }
        }
        Some(Value::Arr(a)) => {
            for e in a {
                let q = e.as_arr()?;
                if q.len() != 2 {
                    return None;
                }
                put(py_str(&q[0]), fnum(&q[1])?);
            }
        }
        Some(Value::Str(s)) if s.is_empty() => {}
        Some(_) => return None,
    }
    Some(out)
}

fn keep_sym(out: &mut Value, old: &Value) {
    if let Some(s) = old.get("sym").and_then(Value::as_str)
        && SYMS.contains(&s)
    {
        out.set("sym", Value::Str(s.to_owned()));
    }
}

fn vars_value(names: &[String], values: &[(String, f64)]) -> Value {
    Value::Obj(
        names
            .iter()
            .map(|n| {
                let v = values.iter().find(|(m, _)| m == n).map_or(1.0, |e| e.1);
                (n.clone(), Value::Num(v))
            })
            .collect(),
    )
}

/// A loop edited by hand from a file made valid, or None.
pub fn clean_loop(c: &Value) -> Option<Value> {
    let pts = pts_of(c.get("pts")?)?;
    let n = pts.len();
    if n < 4 || (n - 1) % 3 != 0 || !pts.iter().all(|q| q[0].is_finite() && q[1].is_finite()) {
        return None;
    }
    let mut out = Value::obj();
    out.set("pts", pts_value(&pts));
    let last = ((n - 1) / 3) as i64;
    let mut sharp = Vec::new();
    if let Some(s) = c.get("sharp") {
        for a in s.as_arr()? {
            let a = inum(a)?;
            if 0 < a && a < last {
                sharp.push(a);
            }
        }
    }
    sharp.sort_unstable();
    sharp.dedup();
    if !sharp.is_empty() {
        out.set(
            "sharp",
            Value::Arr(sharp.into_iter().map(|a| Value::Num(a as f64)).collect()),
        );
    }
    if let Some(s) = c.get("sym").and_then(Value::as_str)
        && matches!(s, "mirror" | "turn" | "flip")
        && (n - 1) % 6 == 0
    {
        out.set("sym", Value::Str(s.to_owned()));
    }
    Some(out)
}

/// The number names in a pattern's formula and its along formula.
pub fn pattern_names(text: &str, along: &str) -> Result<Vec<String>, String> {
    let mut names = Formula::parse(text, true, "x")?.names;
    if !super::formula::py_strip(along).is_empty() {
        for n in Formula::parse(along, true, "x")?.names {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    Ok(names)
}

/// The number names in a shape's two formulas.
pub fn shape_names(x: &str, y: &str) -> Result<Vec<String>, String> {
    let mut names = Formula::parse(x, true, "t")?.names;
    for n in Formula::parse(y, true, "t")?.names {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    Ok(names)
}

fn hand_loop(p: &Value) -> Option<Value> {
    match p.get("loop") {
        Some(l @ Value::Obj(_)) => clean_loop(l),
        _ => None,
    }
}

/// A pattern from a file made valid, or None.
pub fn clean_pattern(p: Option<&Value>) -> Option<Value> {
    let p = p.filter(|p| matches!(p, Value::Obj(_)))?;
    let lp = hand_loop(p);
    let values = vars_of(p)?;
    let (mut text, mut along) = (
        p.get("formula").map_or_else(String::new, py_str),
        p.get("along").map_or_else(String::new, py_str),
    );
    let names = match pattern_names(&text, &along) {
        Ok(n) => n,
        Err(_) => {
            lp.as_ref()?;
            text.clear();
            along.clear();
            Vec::new()
        }
    };
    let mut out = Value::obj();
    out.set(
        "preset",
        Value::Str(p.get("preset").map_or_else(String::new, py_str)),
    );
    out.set("formula", Value::Str(text));
    out.set("vars", vars_value(&names, &values));
    let loops = get_float(p, "loops", LOOPS_DEFAULT)?;
    out.set("loops", Value::Num(loops));
    out.set(
        "each",
        Value::Bool(p.get("each").is_some_and(Value::truthy)),
    );
    let k = get_float(p, "k", 1.0)?;
    out.set("k", Value::Num(k));
    out.set(
        "mirror",
        Value::Bool(p.get("mirror").is_some_and(Value::truthy)),
    );
    let scale = get_float(p, "scale", 1.0)?;
    out.set("scale", Value::Num(scale));
    if !(loops > 0.0 && k > 0.0 && scale.is_finite()) {
        return None;
    }
    if !super::formula::py_strip(&along).is_empty() {
        out.set("along", Value::Str(along));
    }
    if let Some(n) = p.get("name").filter(|n| n.truthy()) {
        out.set("name", Value::Str(py_str(n)));
    }
    if let Some(l) = lp {
        out.set("loop", l);
    }
    keep_sym(&mut out, p);
    Some(out)
}

/// A shape formula from a file made valid, or None.
pub fn clean_shape_formula(p: Option<&Value>) -> Option<Value> {
    let p = p.filter(|p| matches!(p, Value::Obj(_)))?;
    let lp = hand_loop(p);
    let values = vars_of(p)?;
    let (mut tx, mut ty) = (
        p.get("x").map_or_else(String::new, py_str),
        p.get("y").map_or_else(String::new, py_str),
    );
    let names = match shape_names(&tx, &ty) {
        Ok(n) => n,
        Err(_) => {
            lp.as_ref()?;
            tx.clear();
            ty.clear();
            Vec::new()
        }
    };
    let mut out = Value::obj();
    out.set(
        "preset",
        Value::Str(p.get("preset").map_or_else(String::new, py_str)),
    );
    out.set("x", Value::Str(tx));
    out.set("y", Value::Str(ty));
    out.set("vars", vars_value(&names, &values));
    let k = get_float(p, "k", 1.0)?;
    out.set("k", Value::Num(k));
    out.set(
        "mirror",
        Value::Bool(p.get("mirror").is_some_and(Value::truthy)),
    );
    if k.is_nan() || k <= 0.0 {
        return None;
    }
    if let Some(n) = p.get("name").filter(|n| n.truthy()) {
        out.set("name", Value::Str(py_str(n)));
    }
    if let Some(l) = lp {
        out.set("loop", l);
    }
    keep_sym(&mut out, p);
    Some(out)
}

// ------------------------------------------------------------------ loops and shapes

fn sym_of(p: &Value) -> Option<&str> {
    p.get("sym")
        .and_then(Value::as_str)
        .filter(|s| SYMS.contains(s))
}

fn vars_pairs(p: &Value) -> Vec<(String, f64)> {
    match p.get("vars") {
        Some(Value::Obj(o)) => o
            .iter()
            .map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(f64::NAN)))
            .collect(),
        _ => Vec::new(),
    }
}

fn getf(p: &Value, key: &str) -> f64 {
    p.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

/// A formula worked out at each x (Err: it can't be, anywhere).
fn run(f: &Formula, xs: &[f64], vars: &[(String, f64)], var: &str) -> Result<Vec<f64>, String> {
    let vals = f.bind(vars);
    let mut out = Vec::with_capacity(xs.len());
    for &x in xs {
        let y = vals
            .as_ref()
            .ok_or(())
            .and_then(|v| f.eval(x, v).map_err(|_| ()))
            .ok()
            .filter(|y| y.is_finite());
        match y {
            Some(y) => out.push(y),
            None => {
                return Err(format!(
                    "It can't be worked out at {var} = {}.",
                    super::np::pyround(x, 3)
                ));
            }
        }
    }
    Ok(out)
}

/// One loop of the pattern's formula: (along 0 → 1, sideways in keys), with
/// its symmetric halves, not mirrored / scaled (`formula_loop`).
pub fn formula_loop(p: &Value) -> Result<(Vec<f64>, Vec<f64>), String> {
    let text = p.get("formula").and_then(Value::as_str).unwrap_or("");
    let along = p.get("along").and_then(Value::as_str).unwrap_or("");
    let sym = sym_of(p);
    let mut fns = vec![Formula::parse(text, true, "x")?];
    if !super::formula::py_strip(along).is_empty() {
        fns.push(Formula::parse(along, true, "x")?);
    }
    let vars = vars_pairs(p);
    let mut u = linspace(0.0, 1.0, LOOP_SAMPLES + 1);
    let half = LOOP_SAMPLES / 2;
    let xs: &[f64] = if sym.is_some() { &u[..=half] } else { &u };
    let mut got = Vec::new();
    for f in &fns {
        got.push(run(f, xs, &vars, "x")?);
    }
    let mut v = got[0].clone();
    let back: Vec<f64> = v[..v.len() - 1].iter().rev().copied().collect();
    match sym {
        Some("mirror") => v.extend(back),
        Some("turn") => {
            let last = v[v.len() - 1];
            v.extend(back.iter().map(|b| 2.0 * last - b));
        }
        _ => {}
    }
    if got.len() > 1 {
        let mut a = got[1].clone();
        if sym.is_some() {
            let back: Vec<f64> = a[..a.len() - 1].iter().rev().map(|x| -x).collect();
            a.extend(back);
        }
        for (x, d) in u.iter_mut().zip(&a) {
            *x += d;
        }
    }
    Ok((u, v))
}

fn hand_points(p: &Value) -> Option<(Vec<f64>, Vec<f64>)> {
    let l = p.get("loop").filter(|l| l.truthy())?;
    let pts = pts_of(l.get("pts")?)?;
    let a = sample(&pts, 64);
    Some((
        a.iter().map(|q| q[0]).collect(),
        a.iter().map(|q| q[1]).collect(),
    ))
}

/// One loop: the loop edited by hand, else the formula's; mirrored / scaled.
pub fn loop_points(p: &Value) -> Result<(Vec<f64>, Vec<f64>), String> {
    let (u, v) = match hand_points(p) {
        Some(uv) => uv,
        None => formula_loop(p)?,
    };
    let scale = getf(p, "scale");
    let f = if p.get("mirror").is_some_and(Value::truthy) {
        -scale
    } else {
        scale
    };
    Ok((u, v.iter().map(|x| x * f).collect()))
}

/// `fitted_shape`: a drawing fitted to a curve from (0, 0) to (1, 0).
pub fn fitted_shape(a: &[P]) -> Result<(Vec<f64>, Vec<f64>), String> {
    const DOT: &str = "It's just a dot (it has no size).";
    let (lo, hi) = bounds(a);
    let size = hypot(hi[0] - lo[0], hi[1] - lo[1]);
    if size < 1e-12 {
        return Err(DOT.into());
    }
    let n = a.len();
    if dist(a[0], a[n - 1]) < 1e-6 * size {
        // closed: from its leftmost point, as wide as the curve is long
        let mut at = 0;
        for i in 1..n - 1 {
            if a[i][0] < a[at][0] {
                at = i;
            }
        }
        let b: Vec<P> = a[at..n - 1].iter().chain(&a[..=at]).copied().collect();
        let width = hi[0] - lo[0];
        if width < 1e-9 * size {
            return Err(DOT.into());
        }
        let mid = (lo[1] + hi[1]) / 2.0;
        return Ok((
            b.iter().map(|q| (q[0] - lo[0]) / width).collect(),
            b.iter().map(|q| (q[1] - mid) / width).collect(),
        ));
    }
    let d = [a[n - 1][0] - a[0][0], a[n - 1][1] - a[0][1]];
    let length = hypot(d[0], d[1]);
    let (c, s) = (d[0] / length / length, d[1] / length / length);
    let q: Vec<P> = a.iter().map(|p| [p[0] - a[0][0], p[1] - a[0][1]]).collect();
    Ok((
        q.iter().map(|q| q[0] * c + q[1] * s).collect(),
        q.iter().map(|q| q[1] * c - q[0] * s).collect(),
    ))
}

fn bounds(a: &[P]) -> (P, P) {
    let mut lo = a[0];
    let mut hi = a[0];
    for q in a {
        for j in 0..2 {
            if q[j] < lo[j] {
                lo[j] = q[j];
            }
            if q[j] > hi[j] {
                hi[j] = q[j];
            }
        }
    }
    (lo, hi)
}

/// `symmetric_shape`: the second half made from the first, fitted.
pub fn symmetric_shape(a: &[P], sym: &str) -> Result<(Vec<f64>, Vec<f64>), String> {
    let half = SHAPE_SAMPLES / 2;
    let (lo, hi) = bounds(a);
    let size = hypot(hi[0] - lo[0], hi[1] - lo[1]);
    let n = a.len();
    if size > 1e-12 && dist(a[0], a[n - 1]) < 1e-6 * size {
        let first = &a[..=half];
        let (p, m) = (a[0], a[half]);
        let second: Vec<P> = if sym == "turn" {
            let pm = [p[0] + m[0], p[1] + m[1]];
            first[1..]
                .iter()
                .map(|r| [pm[0] - r[0], pm[1] - r[1]])
                .collect()
        } else {
            let d = [m[0] - p[0], m[1] - p[1]];
            let ln = hypot(d[0], d[1]);
            if ln < 1e-9 * size {
                return fitted_shape(a);
            }
            let d = [d[0] / ln, d[1] / ln];
            first[..half]
                .iter()
                .rev()
                .map(|r| {
                    let q = [r[0] - p[0], r[1] - p[1]];
                    // q @ d: OpenBLAS dgemv on x86-64 (fused multiply-add)
                    let w = q[0].mul_add(d[0], q[1] * d[1]);
                    [
                        p[0] + 2.0 * (w * d[0]) - q[0],
                        p[1] + 2.0 * (w * d[1]) - q[1],
                    ]
                })
                .collect()
        };
        let all: Vec<P> = first.iter().copied().chain(second).collect();
        return fitted_shape(&all);
    }
    let (u, v) = fitted_shape(a)?;
    let (u, v) = (&u[..=half], &v[..=half]);
    let (mu, mv) = (u[half], v[half]);
    let mut all: Vec<P> = u.iter().zip(v).map(|(a, b)| [*a, *b]).collect();
    for i in (0..half).rev() {
        let u2 = 2.0 * mu - u[i];
        let v2 = if sym == "turn" { 2.0 * mv - v[i] } else { v[i] };
        all.push([u2, v2]);
    }
    fitted_shape(&all)
}

/// The shape's formulas fitted, not mirrored (`formula_shape`).
pub fn formula_shape(p: &Value) -> Result<(Vec<f64>, Vec<f64>), String> {
    let tx = p.get("x").and_then(Value::as_str).unwrap_or("");
    let ty = p.get("y").and_then(Value::as_str).unwrap_or("");
    let fx = Formula::parse(tx, true, "t")?;
    let fy = Formula::parse(ty, true, "t")?;
    let vars = vars_pairs(p);
    let (bx, by) = (fx.bind(&vars), fy.bind(&vars));
    let ts = linspace(0.0, 1.0, SHAPE_SAMPLES + 1);
    let mut a = Vec::with_capacity(ts.len());
    for &t in &ts {
        let x = bx.as_ref().and_then(|v| fx.eval(t, v).ok());
        let y = by.as_ref().and_then(|v| fy.eval(t, v).ok());
        match (x, y) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => a.push([x, y]),
            _ => {
                return Err(format!(
                    "It can't be worked out at t = {}.",
                    super::np::pyround(t, 3)
                ));
            }
        }
    }
    match sym_of(p) {
        Some(s) => symmetric_shape(&a, s),
        None => fitted_shape(&a),
    }
}

/// The shape as (along, sideways) in the curve's length.
pub fn shape_points(p: &Value) -> Result<(Vec<f64>, Vec<f64>), String> {
    let (u, v) = match hand_points(p) {
        Some(uv) => uv,
        None => formula_shape(p)?,
    };
    if p.get("mirror").is_some_and(Value::truthy) {
        Ok((u, v.iter().map(|x| -x).collect()))
    } else {
        Ok((u, v))
    }
}

// ------------------------------------------------------------------ laying along paths

/// Unit vectors to the left of the path at each point (`_normals`).
fn normals(pts: &[P]) -> Vec<P> {
    let unit = |d: P| {
        let n = np_hypot(d[0], d[1]);
        if n > 1e-12 {
            [d[0] / n, d[1] / n]
        } else {
            [0.0, 0.0]
        }
    };
    let d: Vec<P> = pts
        .windows(2)
        .map(|w| unit([w[1][0] - w[0][0], w[1][1] - w[0][1]]))
        .collect();
    let mut t = Vec::with_capacity(pts.len());
    t.push(d[0]);
    for w in d.windows(2) {
        t.push([w[0][0] + w[1][0], w[0][1] + w[1][1]]);
    }
    t.push(d[d.len() - 1]);
    t.into_iter()
        .map(|v| {
            let u = unit(v);
            [-u[1], u[0]]
        })
        .collect()
}

/// Points at distances `s` along the path moved `v` to its side (`_lay`);
/// before its start / past its end: straight on.
fn lay(pts: &[P], lengths: &[f64], s: &[f64], v: &[f64]) -> Vec<P> {
    let xs: Vec<f64> = pts.iter().map(|q| q[0]).collect();
    let ys: Vec<f64> = pts.iter().map(|q| q[1]).collect();
    let nrm = normals(pts);
    let nxs: Vec<f64> = nrm.iter().map(|q| q[0]).collect();
    let nys: Vec<f64> = nrm.iter().map(|q| q[1]).collect();
    let last = lengths[lengths.len() - 1];
    let (n0, n1) = (nrm[0], nrm[nrm.len() - 1]);
    s.iter()
        .zip(v)
        .map(|(&s, &v)| {
            let mut x = interp(s, lengths, &xs);
            let mut y = interp(s, lengths, &ys);
            let (mut nx, mut ny) = (interp(s, lengths, &nxs), interp(s, lengths, &nys));
            let n = np_hypot(nx, ny);
            let n = if n > 1e-12 { n } else { 1.0 };
            nx /= n;
            ny /= n;
            if s < 0.0 {
                let over = s - lengths[0];
                x += n0[1] * over;
                y += -n0[0] * over;
            }
            if s > last {
                let over = s - last;
                x += n1[1] * over;
                y += -n1[0] * over;
            }
            [x + nx * v, y + ny * v]
        })
        .collect()
}

fn cum_lengths(a: &[P]) -> Vec<f64> {
    let mut ln = vec![0.0];
    let mut acc = 0.0;
    for w in a.windows(2) {
        acc += np_hypot(w[1][0] - w[0][0], w[1][1] - w[0][1]);
        ln.push(acc);
    }
    ln
}

/// The shape laid along each path; unchanged if it can't be worked out.
pub fn shape_paths(paths: Vec<Vec<P>>, p: &Value) -> Vec<Vec<P>> {
    let Ok((u, v)) = shape_points(p) else {
        return paths;
    };
    let k = getf(p, "k");
    paths
        .into_iter()
        .map(|path| {
            let a: Vec<P> = path.iter().map(|q| [q[0] / k, q[1]]).collect();
            if a.len() < 2 {
                return path;
            }
            let ln = cum_lengths(&a);
            let total = ln[ln.len() - 1];
            if total <= 1e-12 {
                return path;
            }
            let s: Vec<f64> = u.iter().map(|x| x * total).collect();
            let w: Vec<f64> = v.iter().map(|x| x * total).collect();
            lay(&a, &ln, &s, &w)
                .into_iter()
                .map(|q| [q[0] * k, q[1]])
                .collect()
        })
        .collect()
}

/// The pattern laid along each path (pieces of one curve); unchanged if the
/// formula can't be worked out.
pub fn pattern_paths(paths: Vec<Vec<P>>, p: &Value) -> Vec<Vec<P>> {
    let Ok((u, v)) = loop_points(p) else {
        return paths;
    };
    let k = getf(p, "k");
    let loops = getf(p, "loops");
    let screens: Vec<Vec<P>> = paths
        .iter()
        .map(|path| path.iter().map(|q| [q[0] / k, q[1]]).collect())
        .collect();
    let lengths: Vec<Vec<f64>> = screens
        .iter()
        .map(|a| {
            if a.len() > 1 {
                cum_lengths(a)
            } else {
                vec![0.0]
            }
        })
        .collect();
    let ends: Vec<f64> = lengths.iter().map(|ln| ln[ln.len() - 1]).collect();
    let spans: Vec<(f64, f64)> = if p.get("each").is_some_and(Value::truthy) {
        ends.iter().map(|&e| (0.0, e)).collect()
    } else {
        let total = super::np::seq_sum(ends.iter().copied());
        let mut start = 0.0;
        ends.iter()
            .map(|&e| {
                let s = start;
                start += e;
                (s, total)
            })
            .collect()
    };
    let back = |a: &[P]| -> Vec<P> { a.iter().map(|q| [q[0] * k, q[1]]).collect() };
    let ut = linspace(0.0, 1.0, u.len());
    let vt = linspace(0.0, 1.0, v.len());
    let mut out = Vec::with_capacity(paths.len());
    for ((a, ln), &(offset, total)) in screens.iter().zip(&lengths).zip(&spans) {
        let size = ln[ln.len() - 1];
        if a.len() < 2 || size <= 1e-12 || total <= 1e-12 {
            out.push(back(a));
            continue;
        }
        let ff = (offset / total * loops).floor();
        let lf = ((offset + size) / total * loops).ceil();
        if !(ff.abs() < 9.0e18 && lf.abs() < 9.0e18) {
            out.push(back(a)); // (Python fails here: NumPy can't count that far)
            continue;
        }
        let (first, last) = (ff as i64, lf as i64);
        let need = a.len() as f64 / {
            let r = loops * size / total;
            if r > 1e-9 { r } else { 1e-9 }
        };
        // max(LOOP_SAMPLES, ceil(need / LOOP_SAMPLES) * LOOP_SAMPLES)
        let c = (need / LOOP_SAMPLES as f64).ceil();
        let ls = LOOP_SAMPLES as i64;
        let mut per: i64 = if c * ls as f64 > ls as f64 {
            if c < 1e12 { c as i64 * ls } else { 1 << 50 }
        } else {
            ls
        };
        per = per.min((MAX_POINTS / (last - first).max(1)).max(8));
        let t = linspace(0.0, 1.0, per as usize + 1);
        let uu: Vec<f64> = t.iter().map(|&x| interp(x, &ut, &u)).collect();
        let vv: Vec<f64> = t.iter().map(|&x| interp(x, &vt, &v)).collect();
        let mut count = last - first;
        if (count as i128) * (per as i128) > MAX_POINTS as i128 {
            count = MAX_POINTS / per;
        }
        let per = per as usize;
        let total_n = count as usize * per + 1;
        let mut s = Vec::with_capacity(total_n);
        let mut side = Vec::with_capacity(total_n);
        for i in 0..count {
            let lno = (first + i) as f64;
            for j in 0..per {
                s.push(uu[j] + lno);
                side.push(vv[j]);
            }
        }
        s.push((first + count - 1) as f64 + uu[per]);
        side.push(vv[per]);
        for x in &mut s {
            *x = *x / loops * total - offset;
        }
        let inside: Vec<usize> = (0..s.len())
            .filter(|&i| s[i] >= 0.0 && s[i] <= size)
            .collect();
        let (Some(&i0), Some(&i1)) = (inside.first(), inside.last()) else {
            out.push(back(a));
            continue;
        };
        let mut s_run: Vec<f64> = s[i0..=i1].to_vec();
        let mut v_run: Vec<f64> = side[i0..=i1].to_vec();
        if i0 > 0 && s[i0] > 0.0 {
            let f = (0.0 - s[i0 - 1]) / (s[i0] - s[i0 - 1]);
            s_run.insert(0, 0.0);
            v_run.insert(0, side[i0 - 1] + (side[i0] - side[i0 - 1]) * f);
        }
        if i1 + 1 < s.len() && s[i1] < size {
            let f = (size - s[i1]) / (s[i1 + 1] - s[i1]);
            s_run.push(size);
            v_run.push(side[i1] + (side[i1 + 1] - side[i1]) * f);
        }
        out.push(back(&lay(a, ln, &s_run, &v_run)));
    }
    out
}

/// Whether the holder (shape / stroke / funnel curve) has formulas.
pub fn has_formula(holder: &Value) -> bool {
    holder.get("shape").is_some_and(Value::truthy)
        || holder.get("pattern").is_some_and(Value::truthy)
}

/// The paths with the holder's shape, then its pattern, on them.
pub fn formed_paths(mut paths: Vec<Vec<P>>, holder: &Value) -> Vec<Vec<P>> {
    if let Some(s) = holder.get("shape").filter(|s| s.truthy()) {
        paths = shape_paths(paths, s);
    }
    if let Some(p) = holder.get("pattern").filter(|p| p.truthy()) {
        paths = pattern_paths(paths, p);
    }
    paths
}

/// One path with the holder's formulas (`rev`: they run from its end).
pub fn formed_path(path: Vec<P>, holder: &Value) -> Vec<P> {
    if !has_formula(holder) {
        return path;
    }
    if holder.get("rev").is_some_and(Value::truthy) {
        let mut r = path;
        r.reverse();
        let mut got = formed_paths(vec![r], holder).swap_remove(0);
        got.reverse();
        return got;
    }
    formed_paths(vec![path], holder).swap_remove(0)
}
