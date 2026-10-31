//! Funnels (funnel.py): lines leading to a wall, opening along curves,
//! filled with spam or long notes. `sh["pts"]` = [line start, line end,
//! wall end 1, wall end 2, (line 2 start, line 2 end, ...)];
//! `sh["starts"]` = curves opening from points on the lines to the wall ends.

use super::custom::{poly_edges, row_spans};
use super::fit::fit;
use super::np::hypot;
use super::paths::{EDGE, P, TOP_KEY, pitch_of};
use super::{curves, fnum, gets, inum, pattern, pts_of, pts_value};
use crate::json::Value;

pub const FUNNEL_FILLS: [&str; 2] = ["spam", "long"];
pub const GATE_CHANGES: [&str; 2] = ["steps", "smooth"];
pub const GATE_FOLLOWS: [&str; 2] = ["time", "curve"];
pub const WALL_MODES: [&str; 2] = ["in", "past"];
pub const FUNNEL_BEND: [f64; 2] = [0.75, 0.2];
pub const MAX_BENDS: usize = 32;
pub const DEFAULT_CURVE: [P; 4] = [[0.0, 0.0], [0.7, 0.06], [0.94, 0.3], [1.0, 1.0]];

#[inline]
fn pmax(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}
#[inline]
fn pmin(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

fn fcmp(a: f64, b: f64) -> std::cmp::Ordering {
    super::shrink::fcmp(a, b)
}

// ================================================================ cleaning

/// The funnel settings (not its lines or curves), made valid.
pub fn clean_funnel(sh: &Value, out: &mut Value) -> Option<()> {
    for (key, choices, d) in [
        ("fill", &FUNNEL_FILLS, "spam"),
        ("change", &GATE_CHANGES, "steps"),
        ("follow", &GATE_FOLLOWS, "time"),
        ("wall", &WALL_MODES, "in"),
    ] {
        let v = gets(sh, key).filter(|v| choices.contains(v)).unwrap_or(d);
        out.set(key, Value::Str(v.to_owned()));
    }
    let mut g = [0.0; 2];
    for (i, key) in ["gate0", "gate1"].iter().enumerate() {
        let v = match sh.get(key) {
            None => 0.0625,
            Some(v) => fnum(v)?,
        };
        g[i] = pmax(1e-6, v);
    }
    let vary = match sh.get("vary") {
        Some(v) => v.truthy(),
        None => (g[0] - g[1]).abs() > 1e-9,
    };
    if !vary {
        g[1] = g[0];
    }
    out.set("gate0", Value::Num(g[0]));
    out.set("gate1", Value::Num(g[1]));
    out.set("vary", Value::Bool(vary));
    Some(())
}

fn clamp_bend(u: f64, f: f64) -> P {
    [pmin(0.99, pmax(0.01, u)), pmin(0.999, pmax(0.001, f))]
}

fn bend_curve(bend: P) -> Option<(f64, bool)> {
    let [u, f] = clamp_bend(bend[0], bend[1]);
    if (u - f).abs() < 1e-4 {
        return None;
    }
    if f < u {
        return Some((f * (1.0 - u) / (u - f), false));
    }
    Some(((1.0 - f) * u / (f - u), true))
}

fn funnel_f(bend: P, u: f64) -> f64 {
    let Some((a, mirrored)) = bend_curve(bend) else {
        return u;
    };
    let u = if mirrored { 1.0 - u } else { u };
    let y = a * u / (1.0 + a - u);
    if mirrored { 1.0 - y } else { y }
}

fn funnel_u(bend: P, y: f64) -> f64 {
    let Some((a, mirrored)) = bend_curve(bend) else {
        return y;
    };
    let y = if mirrored { 1.0 - y } else { y };
    let u = if a + y > 0.0 {
        y * (1.0 + a) / (a + y)
    } else {
        0.0
    };
    if mirrored { 1.0 - u } else { u }
}

fn clean_bends(bends: &[P]) -> Vec<P> {
    let mut v: Vec<P> = bends.iter().map(|b| clamp_bend(b[0], b[1])).collect();
    v.sort_by(|a, b| fcmp(a[0], b[0]).then(fcmp(a[1], b[1])));
    v.truncate(MAX_BENDS);
    let mut out: Vec<P> = Vec::new();
    for b in v {
        if out.last().is_none_or(|l| b[0] > l[0] + 1e-4) {
            out.push(b);
        }
    }
    out
}

/// Monotone cubic through the points (Fritsch-Carlson).
fn smooth_curve(xs: Vec<f64>, ys: Vec<f64>) -> impl Fn(f64) -> f64 {
    let n = xs.len();
    let h: Vec<f64> = (0..n - 1).map(|i| xs[i + 1] - xs[i]).collect();
    let d: Vec<f64> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / h[i]).collect();
    let mut m = vec![0.0; n];
    for i in 1..n - 1 {
        if d[i - 1] * d[i] > 0.0 {
            let (w1, w2) = (2.0 * h[i] + h[i - 1], h[i] + 2.0 * h[i - 1]);
            m[i] = (w1 + w2) / (w1 / d[i - 1] + w2 / d[i]);
        }
    }
    let end_slope = |h0: f64, h1: f64, d0: f64, d1: f64| -> f64 {
        let s = ((2.0 * h0 + h1) * d0 - h0 * d1) / (h0 + h1);
        if s * d0 <= 0.0 {
            return 0.0;
        }
        if d0 * d1 <= 0.0 && s.abs() > 3.0 * d0.abs() {
            return 3.0 * d0;
        }
        s
    };
    m[0] = end_slope(h[0], h[1], d[0], d[1]);
    m[n - 1] = end_slope(h[n - 2], h[n - 3], d[n - 2], d[n - 3]);
    move |u: f64| {
        let i = ((xs.partition_point(|x| *x <= u) as i64 - 1).max(0) as usize).min(n - 2);
        let t = (u - xs[i]) / h[i];
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * ys[i]
            + (t3 - 2.0 * t2 + t) * h[i] * m[i]
            + (3.0 * t2 - 2.0 * t3) * ys[i + 1]
            + (t3 - t2) * h[i] * m[i + 1]
    }
}

fn old_curve_points(bends: &[P]) -> Vec<P> {
    let mut us: Vec<f64> = (0..=400).map(|i| i as f64 / 400.0).collect();
    if bends.len() == 1 {
        us.extend((0..=400).map(|i| funnel_u(bends[0], i as f64 / 400.0)));
        us.sort_by(|a, b| fcmp(*a, *b));
        us.dedup();
        return us.iter().map(|&u| [u, funnel_f(bends[0], u)]).collect();
    }
    let mut xs = vec![0.0];
    xs.extend(bends.iter().map(|b| b[0]));
    xs.push(1.0);
    let mut ys = vec![0.0];
    ys.extend(bends.iter().map(|b| b[1]));
    ys.push(1.0);
    let f = smooth_curve(xs, ys);
    us.iter().map(|&u| [u, f(u)]).collect()
}

fn new_curve(pts: &[P]) -> Value {
    let mut c = Value::obj();
    c.set("pts", pts_value(pts));
    c.set("sharp", Value::Arr(Vec::new()));
    c
}

/// A curve from a file made valid (None stays None).
fn clean_curve(c: &Value) -> Result<Option<Value>, ()> {
    if !c.truthy() {
        return Ok(None);
    }
    if let Some(list) = c.as_arr() {
        let mut bends = Vec::new();
        for b in list {
            let p = b.as_arr().ok_or(())?;
            if p.len() != 2 {
                return Err(());
            }
            bends.push([fnum(&p[0]).ok_or(())?, fnum(&p[1]).ok_or(())?]);
        }
        let pts = old_curve_points(&clean_bends(&bends));
        return Ok(Some(new_curve(&fit(&pts, 0.003, None, None))));
    }
    if !matches!(c, Value::Obj(_)) {
        return Err(());
    }
    let mut pts = match c.get("pts") {
        None => Vec::new(),
        Some(v) => pts_of(v).ok_or(())?,
    };
    if pts.len() < 4 || (pts.len() - 1) % 3 != 0 {
        return Ok(Some(new_curve(&DEFAULT_CURVE)));
    }
    let last = pts.len() - 1;
    pts[0] = [0.0, 0.0];
    pts[last] = [1.0, 1.0];
    let n = curves::anchor_count(pts.len()) as i64;
    let mut sharp: Vec<i64> = match c.get("sharp") {
        None => Vec::new(),
        Some(v) => v
            .as_arr()
            .ok_or(())?
            .iter()
            .map(|a| inum(a).ok_or(()))
            .collect::<Result<Vec<i64>, ()>>()?,
    };
    sharp.retain(|a| 0 < *a && *a < n - 1);
    sharp.sort_unstable();
    sharp.dedup();
    let mut out = Value::obj();
    out.set("pts", pts_value(&pts));
    out.set(
        "sharp",
        Value::Arr(sharp.into_iter().map(|a| Value::Num(a as f64)).collect()),
    );
    if let Some(l) = c.get("link").filter(|l| !matches!(l, Value::Null)) {
        out.set("link", Value::Num(inum(l).ok_or(())? as f64));
        out.set(
            "flip",
            Value::Bool(c.get("flip").is_some_and(Value::truthy)),
        );
    }
    let pat = pattern::clean_pattern(c.get("pattern"));
    let form = pattern::clean_shape_formula(c.get("shape"));
    let any = pat.is_some() || form.is_some();
    if let Some(p) = pat {
        out.set("pattern", p);
    }
    if let Some(f) = form {
        out.set("shape", f);
    }
    if any && c.get("rev").is_some_and(Value::truthy) {
        out.set("rev", Value::Bool(true));
    }
    Ok(Some(out))
}

fn next_link(starts: &[Value]) -> i64 {
    1 + starts
        .iter()
        .flat_map(|st| st.get("ends").and_then(Value::as_arr).unwrap_or(&[]).iter())
        .filter_map(|c| c.get("link").and_then(inum))
        .max()
        .unwrap_or(0)
}

/// The starts from a file, made valid (None: broken).
pub fn clean_starts(starts: Option<&Value>, lines: usize) -> Option<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut old_twins: Vec<usize> = Vec::new();
    let list: &[Value] = match starts {
        None | Some(Value::Null) => &[],
        Some(v) if !v.truthy() => &[],
        Some(v) => v.as_arr()?,
    };
    for st in list {
        if !matches!(st, Value::Obj(_)) {
            return None;
        }
        let mut raw: Vec<Value> = match st.get("ends").filter(|e| e.truthy()) {
            None => Vec::new(),
            Some(e) => e.as_arr()?.to_vec(),
        };
        raw.extend([Value::Null, Value::Null]);
        raw.truncate(2);
        let mut ends = Vec::new();
        for c in &raw {
            ends.push(clean_curve(c).ok()?);
        }
        let line = match st.get("line") {
            None => 0,
            Some(v) => inum(v)?,
        };
        if ends.iter().any(Option::is_some) && 0 <= line && (line as usize) < lines {
            let at = match st.get("at") {
                None => 0.0,
                Some(v) => fnum(v)?,
            };
            let both = ends.iter().all(Option::is_some);
            let mut s = Value::obj();
            s.set("line", Value::Num(line as f64));
            s.set("at", Value::Num(pmin(1.0, pmax(0.0, at))));
            s.set(
                "ends",
                Value::Arr(ends.into_iter().map(|e| e.unwrap_or(Value::Null)).collect()),
            );
            out.push(s);
            if both && raw.iter().any(|c| c.as_arr().is_some()) {
                old_twins.push(out.len() - 1);
            }
        }
    }
    for i in old_twins {
        let link = next_link(&out);
        if let Value::Obj(o) = &mut out[i]
            && let Some((_, Value::Arr(ends))) = o.iter_mut().find(|(k, _)| k == "ends")
        {
            for c in ends.iter_mut() {
                c.set("link", Value::Num(link as f64));
                c.set("flip", Value::Bool(false));
            }
        }
    }
    let key = |s: &Value| {
        (
            s.get("line").and_then(fnum).unwrap_or(0.0),
            s.get("at").and_then(fnum).unwrap_or(0.0),
        )
    };
    out.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        fcmp(ka.0, kb.0).then(fcmp(ka.1, kb.1))
    });
    Some(Value::Arr(out))
}

/// A funnel from the first version ([start, wall top] + "sides") → its line
/// + wall, and its start.
pub fn old_funnel(sh: &Value, pts: &[P]) -> Option<(Vec<P>, Value)> {
    let ([b0, p0], [b1, p1]) = (pts[0], pts[1]);
    let bend = match sh.get("bend").filter(|b| b.truthy()) {
        Some(b) => {
            let a = b.as_arr()?;
            if a.len() != 2 {
                return None;
            }
            clamp_bend(fnum(&a[0])?, fnum(&a[1])?)
        }
        None => clamp_bend(FUNNEL_BEND[0], FUNNEL_BEND[1]),
    };
    let bv = pts_value(&[bend]);
    let one = gets(sh, "sides") == Some("one");
    let (pts, ends) = if one {
        (
            vec![[b0, p0], [b1, p0], [b1, p1], [b1, p0]],
            vec![bv, Value::Null],
        )
    } else {
        let h = (p1 - p0).abs();
        (
            vec![[b0, p0], [b1, p0], [b1, p0 + h], [b1, p0 - h]],
            vec![bv.clone(), bv],
        )
    };
    let mut st = Value::obj();
    st.set("line", Value::Num(0.0));
    st.set("at", Value::Num(0.0));
    st.set("ends", Value::Arr(ends));
    Some((pts, Value::Arr(vec![st])))
}

/// The funnel part of `engine.clean_shape` (None: not a valid funnel).
pub fn clean_shape_funnel(sh: &Value, out: &mut Value) -> Option<()> {
    let mut pts = super::shape_pts(out);
    let mut starts = sh
        .get("starts")
        .filter(|s| !matches!(s, Value::Null))
        .cloned();
    if pts.len() == 2 && starts.is_none() {
        let (p, s) = old_funnel(sh, &pts)?;
        pts = p;
        starts = Some(s);
        out.set("pts", pts_value(&pts));
    }
    if pts.len() < 4 || pts.len() % 2 == 1 {
        return None;
    }
    clean_funnel(sh, out)?;
    let s = clean_starts(starts.as_ref(), pts.len() / 2 - 1)?;
    out.set("starts", s);
    Some(())
}

// ================================================================ geometry

/// A funnel read from its cleaned shape.
#[derive(Clone)]
pub struct Funnel<'a> {
    pub pts: Vec<P>,
    /// (line, at, [curve or None; 2])
    pub starts: Vec<(usize, f64, [Option<&'a Value>; 2])>,
    pub fill: &'a str,
    pub gate0: f64,
    pub gate1: f64,
    pub change: &'a str,
    pub follow: &'a str,
    pub wall: &'a str,
}

impl<'a> Funnel<'a> {
    pub fn new(sh: &'a Value) -> Funnel<'a> {
        let starts = sh
            .get("starts")
            .and_then(Value::as_arr)
            .unwrap_or(&[])
            .iter()
            .map(|st| {
                let line = st.get("line").and_then(inum).unwrap_or(0).max(0) as usize;
                let at = st.get("at").and_then(fnum).unwrap_or(0.0);
                let e = st.get("ends").and_then(Value::as_arr).unwrap_or(&[]);
                let get = |i: usize| e.get(i).filter(|c| c.truthy());
                (line, at, [get(0), get(1)])
            })
            .collect();
        Funnel {
            pts: super::shape_pts(sh),
            starts,
            fill: gets(sh, "fill").unwrap_or("spam"),
            gate0: super::getf(sh, "gate0", 0.0625),
            gate1: super::getf(sh, "gate1", 0.0625),
            change: gets(sh, "change").unwrap_or("steps"),
            follow: gets(sh, "follow").unwrap_or("time"),
            wall: gets(sh, "wall").unwrap_or("in"),
        }
    }

    /// Every line of the funnel (not the wall).
    pub fn lines(&self) -> Vec<(P, P)> {
        let p = &self.pts;
        let mut out = vec![(p[0], p[1])];
        let mut i = 4;
        while i + 1 < p.len() {
            out.push((p[i], p[i + 1]));
            i += 2;
        }
        out
    }

    fn line_index(line: usize) -> usize {
        if line == 0 { 0 } else { 2 + 2 * line }
    }

    fn start_point(&self, at: f64, line: usize) -> P {
        let i = Self::line_index(line);
        let ([b0, p0], [b1, p1]) = (self.pts[i], self.pts[i + 1]);
        [b0 + (b1 - b0) * at, p0 + (p1 - p0) * at]
    }

    /// (S, U, V) of a curve's box, or None when the wall end is on the line.
    pub fn curve_box(&self, at: f64, end: usize, line: usize) -> Option<(P, P, P)> {
        if self.pts.len() < 4 {
            return None;
        }
        let s = self.start_point(at, line);
        let i = Self::line_index(line);
        let ([b0, p0], [b1, p1]) = (self.pts[i], self.pts[i + 1]);
        let (w0, w1) = (self.pts[2], self.pts[3]);
        let e = self.pts[2 + end];
        let (d1, d2) = ([b1 - b0, p1 - p0], [w1[0] - w0[0], w1[1] - w0[1]]);
        let (ex, ey) = (e[0] - s[0], e[1] - s[1]);
        let det = d1[0] * d2[1] - d1[1] * d2[0];
        let lim = 1e-6 * hypot(d1[0], d1[1]) * hypot(d2[0], d2[1]);
        let (u, v) = if det.abs() > lim && lim > 0.0 {
            let x = (ex * d2[1] - ey * d2[0]) / det;
            let y = (d1[0] * ey - d1[1] * ex) / det;
            ([x * d1[0], x * d1[1]], [y * d2[0], y * d2[1]])
        } else {
            ([ex, 0.0], [0.0, ey])
        };
        if (u[0] * v[1] - u[1] * v[0]).abs() < 1e-9 {
            return None;
        }
        Some((s, u, v))
    }

    /// `[(curve points, corner)]` (short: each curve stops just inside its
    /// last key).
    pub fn curves(&self, short: bool) -> Vec<(Vec<P>, P)> {
        let mut out = Vec::new();
        for &(line, at, ends) in &self.starts {
            for (end, c) in ends.iter().enumerate() {
                let Some(c) = c else { continue };
                let Some(mut bx) = self.curve_box(at, end, line) else {
                    continue;
                };
                if short && bx.2[1].abs() > 2.0 * EDGE {
                    let k = 1.0 - EDGE / bx.2[1].abs();
                    bx.2 = [bx.2[0] * k, bx.2[1] * k];
                }
                let corner = box_point(bx, 1.0, 0.0);
                out.push((
                    curve_points(c)
                        .iter()
                        .map(|&[u, f]| box_point(bx, u, f))
                        .collect(),
                    corner,
                ));
            }
        }
        out
    }

    /// The straight parts: every line, then the wall.
    pub fn segments(&self) -> Vec<(P, P)> {
        let lines = self.lines();
        let mut out = vec![lines[0]];
        if self.pts.len() >= 4 {
            out.push((self.pts[2], self.pts[3]));
        }
        out.extend(lines.into_iter().skip(1));
        out
    }

    /// The lines, the wall and every curve, as polylines.
    pub fn strokes(&self) -> Vec<Vec<P>> {
        let mut out: Vec<Vec<P>> = self
            .segments()
            .into_iter()
            .map(|(a, b)| vec![a, b])
            .collect();
        out.extend(self.curves(false).into_iter().map(|(c, _)| c));
        out
    }

    /// The area of every curve: the curve, then back along the wall and line.
    pub fn polys(&self, short: bool) -> Vec<Vec<P>> {
        self.curves(short)
            .into_iter()
            .map(|(mut c, corner)| {
                let first = c[0];
                c.push(corner);
                c.push(first);
                c
            })
            .collect()
    }

    /// Two halves when there are lines on both sides of the wall.
    fn sides(&self) -> Vec<(Funnel<'a>, bool)> {
        let lines = self.lines();
        if self.pts.len() < 4 || lines.len() < 2 {
            return vec![(self.clone(), true)];
        }
        let ([wb0, wp0], [wb1, wp1]) = (self.pts[2], self.pts[3]);
        let side = |l: &(P, P)| {
            let ([b0, p0], [b1, p1]) = *l;
            (wb1 - wb0) * ((p0 + p1) / 2.0 - wp0) - (wp1 - wp0) * ((b0 + b1) / 2.0 - wb0) > 0.0
        };
        let main = side(&lines[0]);
        let mut out = Vec::new();
        for keep in [main, !main] {
            let ns: Vec<usize> = (0..lines.len())
                .filter(|&n| side(&lines[n]) == keep)
                .collect();
            if ns.is_empty() {
                continue;
            }
            let mut pts = vec![lines[ns[0]].0, lines[ns[0]].1, self.pts[2], self.pts[3]];
            for &n in &ns[1..] {
                pts.push(lines[n].0);
                pts.push(lines[n].1);
            }
            let starts = self
                .starts
                .iter()
                .filter_map(|&(line, at, ends)| {
                    ns.iter().position(|&n| n == line).map(|i| (i, at, ends))
                })
                .collect();
            out.push((
                Funnel {
                    pts,
                    starts,
                    ..self.clone()
                },
                keep == main,
            ));
        }
        out
    }

    /// Where each key plays: `[(key, merged [first beat, last beat] list)]`
    /// in the order the keys were first met.
    fn key_spans(&self, main: bool) -> Vec<(i64, Vec<[f64; 2]>)> {
        let mut pieces: Vec<(i64, Vec<[f64; 2]>)> = Vec::new();
        let mut add = |q: i64, s: [f64; 2]| match pieces.iter_mut().find(|(k, _)| *k == q) {
            Some(e) => e.1.push(s),
            None => pieces.push((q, vec![s])),
        };
        let segs = if main { self.segments() } else { self.lines() };
        for (a, b) in segs {
            for q in keys(&[a[1], b[1]]) {
                if let Some(s) = line_band(a, b, q) {
                    add(q, s);
                }
            }
        }
        for poly in self.polys(main && self.wall == "past") {
            let ys: Vec<f64> = poly.iter().map(|p| p[1]).collect();
            let ed = poly_edges(std::slice::from_ref(&poly));
            for q in keys(&ys) {
                for s in row_spans(&ed, q) {
                    add(q, s);
                }
            }
        }
        let mut out = Vec::new();
        for (q, mut spans) in pieces {
            spans.sort_by(|a, b| fcmp(a[0], b[0]).then(fcmp(a[1], b[1])));
            let mut merged: Vec<[f64; 2]> = Vec::new();
            for [a, b] in spans {
                if let Some(l) = merged.last_mut()
                    && a <= l[1] + 1e-9
                {
                    l[1] = pmax(l[1], b);
                    continue;
                }
                merged.push([a, b]);
            }
            if !merged.is_empty() {
                out.push((q, merged));
            }
        }
        out
    }

    /// (t0, sign, length) in beats: where the note grid starts and which way.
    fn axis(&self, spans: &[(i64, Vec<[f64; 2]>)]) -> (f64, f64, f64) {
        let p = &self.pts;
        let rf = if p.len() >= 4 {
            (p[2][0] + p[3][0]) / 2.0
        } else {
            p[1][0]
        };
        if (rf - p[0][0]).abs() > 1e-9 {
            return (
                p[0][0],
                if rf > p[0][0] { 1.0 } else { -1.0 },
                (rf - p[0][0]).abs(),
            );
        }
        let lo = spans
            .iter()
            .flat_map(|(_, s)| s.iter().map(|x| x[0]))
            .fold(f64::INFINITY, pmin);
        let hi = spans
            .iter()
            .flat_map(|(_, s)| s.iter().map(|x| x[1]))
            .fold(f64::NEG_INFINITY, pmax);
        if hi - lo < 1e-9 {
            return (lo, -1.0, self.gate1);
        }
        (lo, 1.0, hi - lo)
    }

    /// Spam: (grid ticks, [(key, first grid line, last grid line)]); long:
    /// (None, [(key, start, end)]).
    fn cells(&self, ppq: f64, main: bool) -> (Option<Vec<i64>>, Vec<[i64; 3]>) {
        let spans = self.key_spans(main);
        if spans.is_empty() {
            return (None, Vec::new());
        }
        let (t0, sign, length) = self.axis(&spans);
        let t0 = t0 * ppq;
        let dist = |beat: f64| sign * (beat * ppq - t0);
        let sorted2 = |a: f64, b: f64| if b < a { [b, a] } else { [a, b] };
        let mut dspans: Vec<(i64, Vec<[f64; 2]>)> = spans
            .iter()
            .map(|(q, s)| {
                (
                    *q,
                    s.iter().map(|x| sorted2(dist(x[0]), dist(x[1]))).collect(),
                )
            })
            .collect();
        let mut walls: Vec<(i64, [f64; 2])> = Vec::new();
        if self.pts.len() >= 4 {
            let (w0, w1) = (self.pts[2], self.pts[3]);
            for q in keys(&[w0[1], w1[1]]) {
                if let Some(s) = line_band(w0, w1, q) {
                    walls.push((q, sorted2(dist(s[0]), dist(s[1]))));
                }
            }
        }
        let wall_of = |q: i64| walls.iter().find(|(k, _)| *k == q).map(|(_, w)| *w);
        if !main {
            dspans = dspans
                .into_iter()
                .filter_map(|(q, s)| match wall_of(q) {
                    Some(w) => {
                        let cut: Vec<[f64; 2]> = s
                            .iter()
                            .filter(|x| x[0] <= w[1])
                            .map(|x| [x[0], pmin(x[1], w[1])])
                            .collect();
                        (!cut.is_empty()).then_some((q, cut))
                    }
                    None => Some((q, s)),
                })
                .collect();
            if dspans.is_empty() {
                return (None, Vec::new());
            }
        }
        let length = pmax(length * ppq, 1.0);
        let past = main && self.wall == "past";
        let g1 = ((self.gate1 * ppq + 0.5).floor() as i64).max(1);
        let at_wall =
            |q: i64, d: f64| wall_of(q).is_some_and(|w| w[0] - 1.0 <= d && d <= w[1] + 1.0);
        let tick = |d: f64| (t0 + sign * d + 0.5).floor() as i64;
        if self.fill == "long" {
            let mut out = Vec::new();
            for (q, s) in &dspans {
                for &[a, b] in s {
                    let far = tick(b)
                        + if past && at_wall(*q, b) {
                            sign as i64 * g1
                        } else {
                            0
                        };
                    let (s0, e0) = if far < tick(a) {
                        (far, tick(a))
                    } else {
                        (tick(a), far)
                    };
                    out.push([*q, s0, e0.max(s0 + 1)]);
                }
            }
            return (None, out);
        }
        let marks = self.grid(ppq, &dspans, length);
        let (mut ticks, mut ds): (Vec<i64>, Vec<f64>) = (Vec::new(), Vec::new());
        for d in marks {
            let t = tick(d);
            if ticks.last() != Some(&t) {
                ticks.push(t);
                ds.push(sign * (t as f64 - t0));
            }
        }
        let nearest = |x: f64| {
            let mut i = ds.partition_point(|v| *v < x);
            if i > 0 && (i == ds.len() || x - ds[i - 1] <= ds[i] - x) {
                i -= 1;
            }
            i
        };
        let mut out = Vec::new();
        for (q, s) in &dspans {
            let mut ranges: Vec<[usize; 2]> = Vec::new();
            for &[a, b] in s {
                let (mut i, mut j) = (nearest(a), nearest(b));
                if past && at_wall(*q, b) {
                    j = (j + 1).min(ds.len() - 1);
                } else if at_wall(*q, b) && j > 0 {
                    i = i.min(j).saturating_sub(1);
                } else if j <= i {
                    let mid = (a + b) / 2.0 - 1e-9;
                    j = ds.partition_point(|v| *v < mid).max(1).min(ds.len() - 1);
                    i = j - 1;
                }
                if let Some(l) = ranges.last_mut()
                    && i <= l[1]
                {
                    l[1] = l[1].max(j);
                    continue;
                }
                ranges.push([i, j]);
            }
            out.extend(ranges.iter().map(|r| [*q, r[0] as i64, r[1] as i64]));
        }
        (Some(ticks), out)
    }

    fn gate_at(&self, g0: f64, g1: f64, w: f64) -> f64 {
        let mut g = g0 * (g1 / g0).powf(w);
        if self.change == "steps" && g0 != g1 {
            g = g0 * 2.0f64.powf(curves::rne((g / g0).log2()));
            g = pmin(pmax(g, pmin(g0, g1)), pmax(g0, g1));
        }
        pmax(g, 1.0)
    }

    /// The note grid every key of a spam funnel shares (grid distances).
    fn grid(&self, ppq: f64, dspans: &[(i64, Vec<[f64; 2]>)], length: f64) -> Vec<f64> {
        let (g0, g1) = (self.gate0 * ppq, self.gate1 * ppq);
        let opened = (self.follow == "curve").then(|| openness(dspans));
        let gate = |d: f64| {
            if d >= length {
                return self.gate_at(g0, g1, 1.0);
            }
            let w = match &opened {
                None => pmin(1.0, pmax(0.0, d / length)),
                Some(o) => o(d),
            };
            self.gate_at(g0, g1, w)
        };
        let lo = dspans
            .iter()
            .flat_map(|(_, s)| s.iter().map(|x| x[0]))
            .fold(f64::INFINITY, pmin);
        let hi = dspans
            .iter()
            .flat_map(|(_, s)| s.iter().map(|x| x[1]))
            .fold(f64::NEG_INFINITY, pmax);
        let mut marks = vec![0.0];
        let mut d = 0.0;
        while d + 1.5 * gate(d) <= length {
            d += gate(d);
            marks.push(d);
        }
        marks.push(length);
        while marks[marks.len() - 1] < hi {
            let l = marks[marks.len() - 1];
            marks.push(l + gate(l));
        }
        let l = marks[marks.len() - 1];
        marks.push(l + gate(l));
        let mut front = Vec::new();
        let mut f = marks[0];
        while f > lo {
            f -= g0;
            front.push(f);
        }
        front.reverse();
        front.extend(marks);
        front
    }
}

fn openness(dspans: &[(i64, Vec<[f64; 2]>)]) -> impl Fn(f64) -> f64 {
    let mut events: Vec<(f64, i64)> = dspans
        .iter()
        .flat_map(|(_, s)| s.iter().map(|x| (x[0], 1)))
        .chain(
            dspans
                .iter()
                .flat_map(|(_, s)| s.iter().map(|x| (x[1], -1))),
        )
        .collect();
    events.sort_by(|a, b| fcmp(a.0, b.0).then(a.1.cmp(&b.1)));
    let (mut xs, mut counts): (Vec<f64>, Vec<i64>) = (Vec::new(), Vec::new());
    let mut c = 0;
    for (x, step) in events {
        c += step;
        if xs.last() == Some(&x) {
            *counts.last_mut().expect("count") = c;
        } else {
            xs.push(x);
            counts.push(c);
        }
    }
    let top = counts.iter().copied().max().unwrap_or(0);
    move |d: f64| {
        let i = xs.partition_point(|x| *x <= d) as i64 - 1;
        let n = if i >= 0 { counts[i as usize] } else { 0 };
        if top > 1 {
            pmin(1.0, pmax(0.0, (n - 1) as f64 / (top - 1) as f64))
        } else {
            0.0
        }
    }
}

fn box_point(bx: (P, P, P), u: f64, f: f64) -> P {
    let ([sb, sp], [ub, up], [vb, vp]) = bx;
    [sb + u * ub + f * vb, sp + u * up + f * vp]
}

/// `[(u, f)]` along a curve in its box, its formulas on it.
fn curve_points(c: &Value) -> Vec<P> {
    let pts = c.get("pts").and_then(pts_of).unwrap_or_default();
    pattern::formed_path(curves::sample(&pts, 64), c)
}

fn keys(ps: &[f64]) -> std::ops::RangeInclusive<i64> {
    let lo = ps.iter().copied().fold(f64::INFINITY, pmin);
    let hi = ps.iter().copied().fold(f64::NEG_INFINITY, pmax);
    pitch_of(lo).max(0)..=pitch_of(hi).min(TOP_KEY)
}

/// (first beat, last beat) where the straight line a → b is on key q.
fn line_band(a: P, b: P, q: i64) -> Option<[f64; 2]> {
    let ([ta, ya], [tb, yb]) = (a, b);
    if ya == yb {
        return (pitch_of(ya) == q).then(|| [pmin(ta, tb), pmax(ta, tb)]);
    }
    let x = (q as f64 - 0.5 - ya) / (yb - ya);
    let y = (q as f64 + 0.5 - ya) / (yb - ya);
    let (u0, u1) = if y < x { (y, x) } else { (x, y) };
    let (u0, u1) = (pmax(u0, 0.0), pmin(u1, 1.0));
    if u0 > u1 {
        return None;
    }
    let (s, e) = (ta + (tb - ta) * u0, ta + (tb - ta) * u1);
    Some(if e < s { [e, s] } else { [s, e] })
}

/// A funnel's `(start, end, key)` notes.
pub fn funnel_notes(sh: &Value, ppq: u32) -> Vec<[i64; 3]> {
    let f = Funnel::new(sh);
    let ppq = f64::from(ppq);
    let mut out = Vec::new();
    for (half, main) in f.sides() {
        let (ticks, cells) = half.cells(ppq, main);
        match ticks {
            None => out.extend(cells.iter().map(|c| [c[1], c[2], c[0]])),
            Some(t) => {
                for c in cells {
                    for k in c[1]..c[2] {
                        let (a, b) = (t[k as usize], t[k as usize + 1]);
                        out.push([a.min(b), a.max(b), c[0]]);
                    }
                }
            }
        }
    }
    out
}

/// How many notes a funnel makes.
pub fn funnel_note_count(sh: &Value, ppq: u32) -> usize {
    let f = Funnel::new(sh);
    f.sides()
        .iter()
        .map(|(half, main)| {
            let (ticks, cells) = half.cells(f64::from(ppq), *main);
            if ticks.is_none() {
                cells.len()
            } else {
                cells.iter().map(|c| (c[2] - c[1]).max(0) as usize).sum()
            }
        })
        .sum()
}
