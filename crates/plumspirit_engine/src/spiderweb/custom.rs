//! Custom shapes (custom.py): drawings from Spiderweb's drawer placed on
//! the roll, as outlines or filled with notes (Fill / Spam / Outline spam),
//! with areas coloured by hand (`areas.rs`), the smallest outline gate
//! (`shrink.rs`), outline colours, "Colours" and pasted notes.
//!
//! A custom shape is strokes in its own box (u, v = 0..1); `sh["pts"]` are
//! three corners of that box on the roll: `[u=0 v=0, u=1 v=0, u=0 v=1]`.

use super::areas::{self, AreaMap, COLOURS, np_i64, npmax, npmin};
use super::hzbass::{self, Hz, KeyGrid};
use super::np::dist;
use super::paths::{
    P, Raw, TOP_KEY, dedupe, keep_longest, line_notes, loop_from_left, parts_notes, pitch_of,
    stretch_ends,
};
use super::shrink::{self, SAMPLES, Seg, fcmp};
use super::{after, curves, fnum, gets, inum, pattern, polygon, pts_of, pts_value, smooth};
use crate::json::Value;
use std::cell::OnceCell;
use std::collections::BTreeMap;

/// `(start, end, key)` in ticks.
pub type N3 = [i64; 3];

pub const FILLS: [&str; 4] = ["empty", "fill", "spam", "outline_spam"];
pub const SPAM_FILLS: [&str; 2] = ["spam", "outline_spam"];
pub const ALIGNS: [&str; 3] = ["auto", "aligned", "centred"];
pub const ENDS: [&str; 5] = ["round", "keep", "drop", "min", "stretch"];
pub const CUSTOM_FLAGS: [&str; 3] = ["union", "apart", "borders"];
pub const ROLES: [&str; 2] = ["edge", "cut"];
const ELLIPSE_STEPS: usize = 360;
const CURVE_STEPS: usize = 240;
/// Loose ends at most this far apart count as touching (joined straight).
const TOUCH_BEATS: f64 = 1.0 / 64.0;
const TOUCH_KEYS: f64 = 1.0;
/// An open part never further than this from its closing line stays unfilled.
const FLAT_KEYS: f64 = 0.5;
const FLAT_BEATS: f64 = 1.0 / 64.0;

/// Python `isinstance(x, int)` for a JSON value (bools are ints). JSON
/// numbers written with a fraction or exponent (`2.0`) also count here —
/// PlumSpirit's JSON keeps no int / float distinction.
pub fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Num(n) if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.2e18 => Some(*n as i64),
        _ => None,
    }
}

fn truthy(sh: &Value, key: &str) -> bool {
    sh.get(key).is_some_and(Value::truthy)
}

/// A stroke's role (None = outline and fill).
pub fn role_of(st: &Value) -> Option<&str> {
    gets(st, "role").filter(|r| ROLES.contains(r))
}

/// A stroke's outline colour (1 ..= 15), 0 = the shape's own.
pub fn colour_of(st: &Value) -> i64 {
    if gets(st, "role") == Some("cut") {
        return 0;
    }
    st.get("colour").and_then(py_int).unwrap_or(0)
}

// ================================================================ cleaning

/// Strokes from a file → valid ones (`clean_strokes`).
pub fn clean_strokes(v: Option<&Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let Some(list) = v.and_then(Value::as_arr) else {
        return out;
    };
    for st in list {
        if !matches!(st, Value::Obj(_)) {
            continue;
        }
        if gets(st, "kind") == Some("ellipse") {
            let Some(b) = st.get("box").and_then(Value::as_arr) else {
                continue;
            };
            let Some(bx) = b.iter().map(fnum).collect::<Option<Vec<f64>>>() else {
                continue;
            };
            if bx.len() == 4 {
                let mut e = Value::obj();
                e.set("kind", Value::Str("ellipse".into()));
                e.set("box", Value::Arr(bx.into_iter().map(Value::Num).collect()));
                out.push(e);
            }
        } else {
            let Some(pts) = st.get("pts").and_then(pts_of) else {
                continue;
            };
            match gets(st, "kind") {
                Some("curve") => {
                    if pts.len() >= 4 {
                        let keep = pts.len() - (pts.len() - 1) % 3;
                        out.push(clean_curve(st, &pts[..keep]));
                    }
                }
                Some("arc") => {
                    if pts.len() == 3 {
                        let mut a = Value::obj();
                        a.set("kind", Value::Str("arc".into()));
                        a.set("pts", pts_value(&pts));
                        a.set("k", Value::Num(curves::arc_k(st.get("k").and_then(fnum))));
                        clean_formulas(st, &mut a);
                        out.push(a);
                    }
                }
                _ => {
                    if !pts.is_empty() {
                        let mut p = Value::obj();
                        p.set("kind", Value::Str("poly".into()));
                        p.set("pts", pts_value(&pts));
                        if truthy(st, "free") {
                            p.set("free", Value::Bool(true));
                            let lv = match st.get("smooth") {
                                None => Some(0.0),
                                Some(v) => fnum(v),
                            };
                            p.set("smooth", Value::Num(smooth::clean_level(lv)));
                            p.set("k", Value::Num(curves::arc_k(st.get("k").and_then(fnum))));
                        } else {
                            if truthy(st, "sides") {
                                p.set("sides", Value::Bool(true));
                            }
                            clean_formulas(st, &mut p);
                        }
                        out.push(p);
                    }
                }
            }
        }
        // (Python sets these on the last stroke kept, even when this one was dropped)
        if let Some(src) = st.get("src").and_then(py_int)
            && let Some(last) = out.last_mut()
        {
            last.set("src", Value::Num(src as f64));
        }
        if let Some(role) = role_of(st).map(str::to_owned)
            && let Some(last) = out.last_mut()
        {
            last.set("role", Value::Str(role));
        }
        if let Some(c) = st.get("colour").and_then(py_int)
            && (1..=COLOURS).contains(&c)
            && let Some(last) = out.last_mut()
        {
            last.set("colour", Value::Num(c as f64));
        }
    }
    out
}

fn clean_curve(st: &Value, pts: &[P]) -> Value {
    let mut out = Value::obj();
    out.set("kind", Value::Str("curve".into()));
    out.set("pts", pts_value(pts));
    let last = (pts.len() as i64 - 1) / 3;
    let sharp: Vec<i64> = match st.get("sharp") {
        None => Vec::new(),
        Some(v) => v
            .as_arr()
            .and_then(|a| a.iter().map(inum).collect::<Option<Vec<i64>>>())
            .unwrap_or_default(),
    };
    let mut v: Vec<i64> = sharp.into_iter().filter(|a| 0 < *a && *a < last).collect();
    v.sort_unstable();
    v.dedup();
    if !v.is_empty() {
        out.set(
            "sharp",
            Value::Arr(v.into_iter().map(|a| Value::Num(a as f64)).collect()),
        );
    }
    if let Some(sym) = gets(st, "sym")
        && matches!(sym, "mirror" | "turn")
        && last % 2 == 0
    {
        out.set("sym", Value::Str(sym.to_owned()));
    }
    clean_formulas(st, &mut out);
    out
}

fn clean_formulas(st: &Value, out: &mut Value) {
    if let Some(p) = pattern::clean_pattern(st.get("pattern")) {
        out.set("pattern", p);
    }
    if let Some(f) = pattern::clean_shape_formula(st.get("shape")) {
        out.set("shape", f);
    }
}

/// The custom-shape part of `engine.clean_shape` (`out` already has the
/// common fields). None: not a valid custom shape.
pub fn clean_custom(sh: &Value, out: &mut Value) -> Option<()> {
    let strokes = clean_strokes(sh.get("strokes"));
    let n_pts = out
        .get("pts")
        .and_then(Value::as_arr)
        .map_or(0, <[Value]>::len);
    if n_pts != 3 || strokes.is_empty() {
        return None;
    }
    out.set(
        "name",
        Value::Str(sh.get("name").map_or_else(String::new, pattern::py_str)),
    );
    out.set("strokes", Value::Arr(strokes));
    let areas = areas::clean_areas(sh.get("areas"));
    if !areas.is_empty() {
        out.set("areas", areas::areas_value(&areas));
    }
    let fill = gets(sh, "fill")
        .filter(|f| FILLS.contains(f))
        .unwrap_or("empty");
    out.set("fill", Value::Str(fill.to_owned()));
    let gate = match sh.get("gate") {
        None => 0.0625,
        Some(v) => fnum(v)?,
    };
    out.set("gate", Value::Num(if gate > 1e-6 { gate } else { 1e-6 }));
    let align = gets(sh, "align")
        .filter(|f| ALIGNS.contains(f))
        .unwrap_or("auto");
    out.set("align", Value::Str(align.to_owned()));
    let ends = gets(sh, "ends")
        .filter(|f| ENDS.contains(f))
        .unwrap_or("drop");
    out.set("ends", Value::Str(ends.to_owned()));
    for k in CUSTOM_FLAGS {
        if sh.get(k) == Some(&Value::Bool(true)) {
            out.set(k, Value::Bool(true));
        }
    }
    let edge = sh
        .get("edge")
        .filter(|e| e.truthy())
        .and_then(fnum)
        .unwrap_or(0.0);
    if edge > 0.0 {
        out.set("edge", Value::Num(if 1e4 < edge { 1e4 } else { edge }));
    }
    if gets(sh, "edge_mode") == Some("sideways") {
        out.set("edge_mode", Value::Str("sideways".into()));
    }
    if let Some(hz) = Hz::clean(sh.get("hz")) {
        out.set("hz", hz.to_value());
    }
    if let Some(fr) = sh.get("from")
        && let Some(olds) = fr.get("shapes").and_then(Value::as_arr)
        && !olds.is_empty()
    {
        let cleaned: Vec<Option<Value>> = olds
            .iter()
            .map(|o| {
                if matches!(o, Value::Obj(_)) {
                    super::clean_shape(o)
                } else {
                    None
                }
            })
            .collect();
        if cleaned.iter().all(Option::is_some)
            && let Some(fp) = fr.get("pts").and_then(pts_of)
            && fp.len() == 3
            && fr.has("strokes")
        {
            let mut f = Value::obj();
            f.set(
                "shapes",
                Value::Arr(cleaned.into_iter().flatten().collect()),
            );
            f.set("strokes", Value::Arr(clean_strokes(fr.get("strokes"))));
            f.set("pts", pts_value(&fp));
            out.set("from", f);
        }
    }
    if let Some(pg) = polygon::clean_polygon(sh.get("polygon"))
        && !sh.has("notes")
        && !truthy(sh, "text")
    {
        out.set("strokes", Value::Arr(polygon::polygon_strokes(&pg)));
        out.set("polygon", pg);
    }
    if let Some(tx) = sh.get("text").filter(|t| matches!(t, Value::Obj(_)))
        && let Some(tx) = super::text::clean_text(tx)
    {
        out.set("text", tx);
    }
    if let Some(notes) = sh.get("notes") {
        let ok = notes.as_str().is_some_and(check_notes);
        if !ok {
            return None;
        }
        out.set("notes", notes.clone());
        out.set("strokes", Value::Arr(vec![box_stroke()]));
        out.set("fill", Value::Str("empty".into()));
        if truthy(sh, "own_vel") {
            out.set("own_vel", Value::Bool(true));
        }
    }
    Some(())
}

/// The box's outline: the strokes of a pasted-notes shape.
pub fn box_stroke() -> Value {
    let mut st = Value::obj();
    st.set("kind", Value::Str("poly".into()));
    st.set(
        "pts",
        pts_value(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]]),
    );
    st
}

// ================================================================ strokes

/// A stroke as (u, v) points.
pub fn stroke_points(st: &Value) -> Vec<P> {
    let pts = st.get("pts").and_then(pts_of).unwrap_or_default();
    match gets(st, "kind") {
        Some("ellipse") => {
            let b: Vec<f64> = st
                .get("box")
                .and_then(Value::as_arr)
                .map(|a| a.iter().map(|x| fnum(x).unwrap_or(0.0)).collect())
                .unwrap_or_else(|| vec![0.0; 4]);
            let (u0, v0, u1, v1) = (b[0], b[1], b[2], b[3]);
            let (cu, cv, ru, rv) = (
                (u0 + u1) / 2.0,
                (v0 + v1) / 2.0,
                (u1 - u0) / 2.0,
                (v1 - v0) / 2.0,
            );
            let mut out: Vec<P> = (0..ELLIPSE_STEPS)
                .map(|i| {
                    let a = 2.0 * std::f64::consts::PI * i as f64 / ELLIPSE_STEPS as f64;
                    [cu - ru * a.cos(), cv + rv * a.sin()]
                })
                .collect();
            out.push(out[0]);
            out
        }
        Some("curve") => pattern::formed_path(curves::sample(&pts, CURVE_STEPS), st),
        Some("arc") => pattern::formed_path(
            curves::arc_points(&pts, st.get("k").and_then(fnum).unwrap_or(1.0)),
            st,
        ),
        _ => {
            let level = st.get("smooth").and_then(fnum).unwrap_or(0.0);
            if truthy(st, "smooth") {
                return smooth::smooth_path(&pts, level, st.get("k").and_then(fnum).unwrap_or(1.0));
            }
            if truthy(st, "sides") && pattern::has_formula(st) {
                let mut out: Vec<P> = Vec::new();
                for side in pattern::formed_paths(polygon::side_paths(&pts), st) {
                    if out.is_empty() {
                        out.extend(side);
                    } else {
                        out.extend(side.into_iter().skip(1));
                    }
                }
                return out;
            }
            if !truthy(st, "free") && pattern::has_formula(st) {
                return pattern::formed_path(pts, st);
            }
            pts
        }
    }
}

pub fn path_closed(path: &[P]) -> bool {
    path.len() >= 3 && dist(path[0], path[path.len() - 1]) < 1e-6
}

/// Point lists that meet end to end become one list.
pub fn join_paths(paths: Vec<Vec<P>>) -> Vec<Vec<P>> {
    let meet = |a: P, b: P| dist(a, b) < 1e-6;
    let (mut done, mut lines): (Vec<Vec<P>>, Vec<Vec<P>>) =
        paths.into_iter().partition(|p| path_closed(p));
    'again: loop {
        for i in 0..lines.len() {
            for j in 0..lines.len() {
                if i == j || lines[i].is_empty() || lines[j].is_empty() {
                    continue;
                }
                let (a, b) = (&lines[i], &lines[j]);
                let (a0, a1, b0, b1) = (a[0], a[a.len() - 1], b[0], b[b.len() - 1]);
                let new: Vec<P> = if meet(a1, b0) {
                    a.iter().chain(b.iter().skip(1)).copied().collect()
                } else if meet(a1, b1) {
                    a.iter().chain(b.iter().rev().skip(1)).copied().collect()
                } else if meet(a0, b0) {
                    a.iter().rev().chain(b.iter().skip(1)).copied().collect()
                } else {
                    continue;
                };
                lines[i] = new;
                lines.remove(j);
                continue 'again;
            }
        }
        break;
    }
    done.extend(lines);
    done
}

/// Python `max(a, b)` for floats.
#[inline]
fn pymax(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

fn near_ends(p: P, q: P) -> bool {
    (p[0] - q[0]).abs() <= TOUCH_BEATS + 1e-9 && (p[1] - q[1]).abs() <= TOUCH_KEYS + 1e-9
}

/// The open path never gets further than half a key (or 1/64 beat
/// sideways) from the straight line between its ends.
fn flat_path(a: &[P]) -> bool {
    let (b0, p0) = (a[0][0], a[0][1]);
    let l = a[a.len() - 1];
    let (db, dp) = (l[0] - b0, l[1] - p0);
    for (along, across, lim, d_along, d_across, s) in [
        (0, 1, FLAT_KEYS, db, dp, b0),
        (1, 0, FLAT_BEATS, dp, db, p0),
    ] {
        if d_along.abs() < 1e-12 {
            continue;
        }
        let t: Vec<f64> = a.iter().map(|q| (q[along] - s) / d_along).collect();
        if t.iter().any(|&t| !(-1e-9..=1.0 + 1e-9).contains(&t)) {
            continue;
        }
        let ok = a.iter().zip(&t).all(|(q, &t)| {
            let line = a[0][across] + t * d_across;
            (q[across] - line).abs() <= lim + 1e-9
        });
        if ok {
            return true;
        }
    }
    db.abs() < 1e-12 && dp.abs() < 1e-12
}

/// How Fill / Spam see the outline: closed loops, closing lines, flat parts.
pub struct Plan {
    pub polys: Vec<Vec<P>>,
    pub closers: Vec<[P; 2]>,
    pub flat: Vec<Vec<P>>,
}

/// Notes along these (joined) paths, like lines.
pub fn paths_outline(paths: &[Vec<P>], ppq: u32) -> Vec<N3> {
    let ppqf = f64::from(ppq);
    let mut raw: Vec<Raw> = Vec::new();
    for path in paths {
        let closed = path_closed(path);
        let path = dedupe(path);
        if path.is_empty() {
            continue;
        }
        if path.len() < 2 {
            let t = (path[0][0] * ppqf + 0.5).floor() as i64;
            raw.push([t, t + 1, pitch_of(path[0][1])]);
            continue;
        }
        let path = if closed {
            let mut p = path;
            if p[p.len() - 1] != p[0] {
                p.push(p[0]);
            }
            loop_from_left(&p)
        } else {
            let mut p = path;
            if p[p.len() - 1][0] < p[0][0] {
                p.reverse();
            }
            stretch_ends(&p, false)
        };
        let scaled: Vec<P> = path.iter().map(|q| [q[0] * ppqf, q[1]]).collect();
        raw.extend(line_notes(&scaled, false));
    }
    keep_longest(&raw)
}

/// Edges of closed polygons without the level ones: `(xa, ya, xb, yb)`.
#[derive(Clone, Default)]
pub struct Edges {
    pub xa: Vec<f64>,
    pub ya: Vec<f64>,
    pub xb: Vec<f64>,
    pub yb: Vec<f64>,
    /// loop number (`row_pieces`; −1 = a fill line)
    pub lid: Vec<i64>,
}

impl Edges {
    fn push(&mut self, a: P, b: P, lid: i64) {
        self.xa.push(a[0]);
        self.ya.push(a[1]);
        self.xb.push(b[0]);
        self.yb.push(b[1]);
        self.lid.push(lid);
    }
    fn len(&self) -> usize {
        self.xa.len()
    }
}

pub fn poly_edges(polys: &[Vec<P>]) -> Edges {
    let mut e = Edges::default();
    for p in polys {
        if p.len() > 1 {
            for w in p.windows(2) {
                if w[0][1] != w[1][1] {
                    e.push(w[0], w[1], 0);
                }
            }
        }
    }
    e
}

/// `np.unique` of floats (sorted, equal ones once).
fn unique_f(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| fcmp(*a, *b));
    v.dedup_by(|a, b| a == b);
    v
}

/// The slices of row q every edge runs through (`row_spans` /
/// `row_pieces`): for each (edge, gap) pair in edge order.
struct Slices {
    levels: Vec<f64>,
    /// (index into the edges, gap)
    pairs: Vec<(usize, usize)>,
}

fn slices(ed: &Edges, here: &[usize], lo: f64, hi: f64) -> Slices {
    let mut ys = vec![lo, hi];
    for &i in here {
        if ed.ya[i] > lo && ed.ya[i] < hi {
            ys.push(ed.ya[i]);
        }
    }
    for &i in here {
        if ed.yb[i] > lo && ed.yb[i] < hi {
            ys.push(ed.yb[i]);
        }
    }
    let levels = unique_f(ys);
    let ss = |v: f64| levels.partition_point(|l| fcmp(*l, v) == std::cmp::Ordering::Less);
    let mut pairs = Vec::new();
    for &i in here {
        let first = ss(npmax(npmin(ed.ya[i], ed.yb[i]), lo));
        let last = ss(npmin(npmax(ed.ya[i], ed.yb[i]), hi));
        for g in first..last.max(first) {
            pairs.push((i, g));
        }
    }
    Slices { levels, pairs }
}

#[inline]
fn x_at(ed: &Edges, i: usize, y: f64) -> f64 {
    ed.xa[i] + (ed.xb[i] - ed.xa[i]) * (y - ed.ya[i]) / (ed.yb[i] - ed.ya[i])
}

fn rows_here(ed: &Edges, lo: f64, hi: f64) -> Vec<usize> {
    (0..ed.len())
        .filter(|&i| npmin(ed.ya[i], ed.yb[i]) < hi && npmax(ed.ya[i], ed.yb[i]) > lo)
        .collect()
}

/// Time ranges (beats) where the closed polygons' inside (even-odd)
/// touches pitch row q anywhere between its bottom and top edge.
pub fn row_spans(ed: &Edges, q: i64) -> Vec<[f64; 2]> {
    let (lo, hi) = (q as f64 - 0.5, q as f64 + 0.5);
    let here = rows_here(ed, lo, hi);
    if here.is_empty() {
        return Vec::new();
    }
    let sl = slices(ed, &here, lo, hi);
    let lv = &sl.levels;
    // (gap, x at the middle, left, right) in lexsort((xm, gap)) order
    let mut items: Vec<(usize, f64, f64, f64)> = sl
        .pairs
        .iter()
        .map(|&(i, g)| {
            let (y0, y1) = (lv[g], lv[g + 1]);
            let mid = (y0 + y1) / 2.0;
            let (x0, x1) = (x_at(ed, i, y0), x_at(ed, i, y1));
            (g, x_at(ed, i, mid), npmin(x0, x1), npmax(x0, x1))
        })
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0).then(fcmp(a.1, b.1)));
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut k = 0;
    while k < items.len() {
        let mut e = k;
        while e < items.len() && items[e].0 == items[k].0 {
            e += 1;
        }
        let mut j = k;
        while j + 1 < e {
            a.push(items[j].2);
            b.push(items[j + 1].3);
            j += 2;
        }
        k = e;
    }
    merge_spans(&a, &b)
}

/// Ranges a[i]..b[i] → `[[start, end]]` in order, touching / overlapping
/// ones made one.
pub fn merge_spans(a: &[f64], b: &[f64]) -> Vec<[f64; 2]> {
    if a.is_empty() {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..a.len()).collect();
    order.sort_by(|&i, &j| fcmp(a[i], a[j]).then(fcmp(b[i], b[j])));
    let mut out: Vec<[f64; 2]> = Vec::new();
    let mut run = f64::NEG_INFINITY;
    let mut first = true;
    for &i in &order {
        if first || a[i] > run {
            out.push([a[i], b[i]]);
        } else {
            let l = out.last_mut().expect("open");
            l[1] = npmax(l[1], b[i]);
        }
        run = if first { b[i] } else { npmax(run, b[i]) };
        first = false;
    }
    out
}

// ================================================================ the shape

/// What the areas fill per sample line, the area edges, the proportion.
type FilledLines = (Vec<Vec<[f64; 2]>>, Vec<Seg>, f64);

/// One custom shape with what's worked out from it (each part once).
pub struct Custom<'a> {
    pub sh: &'a Value,
    pub ppq: u32,
    strokes: Vec<&'a Value>,
    paths: OnceCell<Vec<Vec<P>>>,
    plan: OnceCell<Plan>,
    spans: OnceCell<Vec<N3>>,
    amap: OnceCell<Option<AreaMap>>,
    aspans: OnceCell<Vec<[i64; 4]>>,
    inner: OnceCell<Vec<N3>>,
    gate: OnceCell<Gate>,
}

impl<'a> Custom<'a> {
    pub fn new(sh: &'a Value, ppq: u32) -> Custom<'a> {
        let strokes = sh
            .get("strokes")
            .and_then(Value::as_arr)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        Custom {
            sh,
            ppq,
            strokes,
            paths: OnceCell::new(),
            plan: OnceCell::new(),
            spans: OnceCell::new(),
            amap: OnceCell::new(),
            aspans: OnceCell::new(),
            inner: OnceCell::new(),
            gate: OnceCell::new(),
        }
    }

    fn fill(&self) -> &str {
        gets(self.sh, "fill").unwrap_or("empty")
    }

    /// A text shape (text.rs).
    fn text(&self) -> Option<&Value> {
        self.sh.get("text").filter(|t| t.truthy())
    }

    fn flag(&self, k: &str) -> bool {
        truthy(self.sh, k)
    }

    fn sideways(&self) -> bool {
        gets(self.sh, "edge_mode") == Some("sideways")
    }

    fn frame(&self) -> [P; 3] {
        let p = super::shape_pts(self.sh);
        if p.len() == 3 {
            [p[0], p[1], p[2]]
        } else {
            [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]
        }
    }

    /// The strokes on the roll, (beat, pitch) (`custom_strokes`).
    pub fn paths(&self) -> &Vec<Vec<P>> {
        self.paths.get_or_init(|| {
            if self.text().is_some() {
                return super::text::text_polys(self.sh);
            }
            let [[b0, p0], [b1, p1], [b2, p2]] = self.frame();
            let (ub, up, vb, vp) = (b1 - b0, p1 - p0, b2 - b0, p2 - p0);
            self.strokes
                .iter()
                .map(|st| {
                    stroke_points(st)
                        .iter()
                        .map(|&[u, v]| [b0 + u * ub + v * vb, p0 + u * up + v * vp])
                        .collect()
                })
                .collect()
        })
    }

    /// The paths of strokes with this role.
    fn role_paths(&self, role: Option<&str>) -> Vec<Vec<P>> {
        if self.text().is_some() {
            return if role.is_none() {
                self.paths().clone()
            } else {
                Vec::new()
            };
        }
        self.strokes
            .iter()
            .zip(self.paths())
            .filter(|(st, _)| role_of(st) == role)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// `fill_plan`.
    pub fn plan(&self) -> &Plan {
        self.plan.get_or_init(|| {
            let paths = join_paths(self.role_paths(None));
            let (mut polys, mut opens): (Vec<Vec<P>>, Vec<Vec<P>>) =
                paths.into_iter().partition(|p| path_closed(p));
            let mut closers: Vec<[P; 2]> = Vec::new();
            loop {
                let mut best: Option<(f64, usize, usize, u8, P, P)> = None;
                for i in 0..opens.len() {
                    for j in i..opens.len() {
                        let (a, b) = (&opens[i], &opens[j]);
                        if a.is_empty() || b.is_empty() {
                            continue;
                        }
                        let (a0, a1, b0, b1) = (a[0], a[a.len() - 1], b[0], b[b.len() - 1]);
                        let pairs: &[(P, P, u8)] = if i == j {
                            &[(a1, a0, 0)]
                        } else {
                            &[(a1, b0, 1), (a1, b1, 2), (a0, b0, 3), (a0, b1, 4)]
                        };
                        for &(p, q, how) in pairs {
                            if i == j && a.len() < 3 {
                                continue;
                            }
                            if near_ends(p, q) {
                                let d = pymax(
                                    (p[0] - q[0]).abs() / TOUCH_BEATS,
                                    (p[1] - q[1]).abs() / TOUCH_KEYS,
                                );
                                if best.is_none_or(|b| d < b.0) {
                                    best = Some((d, i, j, how, p, q));
                                }
                            }
                        }
                    }
                }
                let Some((_, i, j, how, p, q)) = best else {
                    break;
                };
                closers.push([p, q]);
                if how == 0 {
                    let mut a = opens.remove(i);
                    a.push(a[0]);
                    polys.push(a);
                    continue;
                }
                let mut a = opens[i].clone();
                let mut b = opens[j].clone();
                if !matches!(how, 1 | 2) {
                    a.reverse();
                }
                if !matches!(how, 1 | 3) {
                    b.reverse();
                }
                a.extend(b);
                opens[i] = a;
                opens.remove(j);
            }
            let mut flat = Vec::new();
            for path in opens {
                if flat_path(&path) {
                    flat.push(path);
                } else {
                    closers.push([path[path.len() - 1], path[0]]);
                    let mut p = path;
                    p.push(p[0]);
                    polys.push(p);
                }
            }
            Plan {
                polys,
                closers,
                flat,
            }
        })
    }

    /// The straight lines closing gaps for Fill / Spam (drawn dashed).
    pub fn gap_lines(&self) -> Vec<[P; 2]> {
        if self.text().is_some() {
            return Vec::new();
        }
        self.plan().closers.clone()
    }

    /// Outline notes along every stroke (only: these strokes); fill lines
    /// make none.
    pub fn outline_notes(&self, only: Option<&[usize]>) -> Vec<N3> {
        let paths = self.paths();
        let all: Vec<usize> = (0..paths.len()).collect();
        let ks = only.unwrap_or(&all);
        let text = self.text().is_some();
        let ps: Vec<Vec<P>> = ks
            .iter()
            .filter(|&&k| text || role_of(self.strokes[k]) != Some("cut"))
            .map(|&k| paths[k].clone())
            .collect();
        paths_outline(&join_paths(ps), self.ppq)
    }

    /// Every stretch of every key inside the shape, `(start, end, key)`
    /// (`inside_spans`, in its order).
    pub fn inside_spans(&self) -> &Vec<N3> {
        self.spans.get_or_init(|| {
            let tx = self.text();
            let polys = if tx.is_some() {
                self.paths()
            } else {
                &self.plan().polys
            };
            let ps: Vec<f64> = polys.iter().flatten().map(|p| p[1]).collect();
            if ps.is_empty() {
                return Vec::new();
            }
            let union = self.flag("union") && tx.is_none();
            let ppqf = f64::from(self.ppq);
            let threshold = tx.map(|t| t.get("threshold").and_then(fnum).unwrap_or(50.0));
            let lo_p = ps
                .iter()
                .copied()
                .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
            let hi_p = ps
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, |a, b| if b > a { b } else { a });
            let per: Vec<Edges> = if union {
                polys
                    .iter()
                    .map(|p| poly_edges(std::slice::from_ref(p)))
                    .collect()
            } else {
                Vec::new()
            };
            let all = if union || tx.is_some() {
                Edges::default()
            } else {
                poly_edges(polys)
            };
            let mut out = Vec::new();
            for q in pitch_of(lo_p).max(0)..=pitch_of(hi_p).min(TOP_KEY) {
                let got = if let Some(t) = threshold {
                    super::text::threshold_spans(polys, q, t)
                } else if union {
                    let mut a = Vec::new();
                    let mut b = Vec::new();
                    for e in &per {
                        for s in row_spans(e, q) {
                            a.push(s[0]);
                            b.push(s[1]);
                        }
                    }
                    merge_spans(&a, &b)
                } else {
                    row_spans(&all, q)
                };
                for [a, b] in got {
                    let s = (a * ppqf + 0.5).floor() as i64;
                    let e = ((b * ppqf + 0.5).floor() as i64).max(s + 1);
                    out.push([s, e, q]);
                }
            }
            out
        })
    }

    // ------------------------------------------------------------ areas

    /// Fill / Spam with areas coloured by hand.
    pub fn has_areas(&self) -> bool {
        truthy(self.sh, "areas")
            && matches!(self.fill(), "fill" | "spam")
            && self.text().is_none()
            && !self.sh.has("notes")
    }

    fn areas(&self) -> Vec<(f64, f64, i64)> {
        areas::clean_areas(self.sh.get("areas"))
    }

    /// (beat, pitch) points → (u, v) in the shape's box (None: flat box).
    pub fn uv_points(&self, path: &[P]) -> Option<Vec<P>> {
        let [[b0, p0], [b1, p1], [b2, p2]] = self.frame();
        let (ub, up, vb, vp) = (b1 - b0, p1 - p0, b2 - b0, p2 - p0);
        let det = ub * vp - up * vb;
        if det.abs() < 1e-12 {
            return None;
        }
        Some(
            path.iter()
                .map(|q| {
                    let (db, dp) = (q[0] - b0, q[1] - p0);
                    [(db * vp - dp * vb) / det, (ub * dp - up * db) / det]
                })
                .collect(),
        )
    }

    /// Lines that split areas without closing a loop: fill lines and flat
    /// outline parts.
    fn area_cuts(&self) -> Vec<Vec<P>> {
        let mut v = self.role_paths(Some("cut"));
        v.extend(self.plan().flat.iter().cloned());
        v
    }

    /// The AreaMap in the box's u, v (None: flat box).
    pub fn shape_areas(&self) -> Option<&AreaMap> {
        self.amap
            .get_or_init(|| {
                self.uv_points(&[[0.0, 0.0]])?;
                let loops: Vec<Vec<P>> = self
                    .plan()
                    .polys
                    .iter()
                    .map(|p| self.uv_points(p).unwrap_or_default())
                    .collect();
                let cuts: Vec<Vec<P>> = self
                    .area_cuts()
                    .iter()
                    .map(|p| self.uv_points(p).unwrap_or_default())
                    .collect();
                Some(AreaMap::new(&loops, &cuts))
            })
            .as_ref()
    }

    /// Each area's colour as given by hand (−1 as normal, 0 empty, k colour
    /// k); one more at the end for walls.
    fn area_paint(&self, amap: &AreaMap) -> Vec<i64> {
        let mut paint = vec![-1i64; amap.count + 1];
        let areas = self.areas();
        for (u, v, c) in areas {
            let lab = amap.at(u, v);
            if lab >= 0 {
                paint[lab as usize] = c;
            }
        }
        paint
    }

    /// Each area's (filled, colour) as Fill / Spam make it (one more at the
    /// end for walls: not filled, −1).
    fn area_state(&self, amap: &AreaMap) -> (Vec<bool>, Vec<i64>) {
        let paint = self.area_paint(amap);
        let loops: Vec<Vec<P>> = self
            .plan()
            .polys
            .iter()
            .map(|p| self.uv_points(p).unwrap_or_default())
            .collect();
        let inside = areas::inside_loops(&amap.spots, &loops, self.flag("union"));
        let mut filled: Vec<bool> = (0..amap.count)
            .map(|i| {
                if paint[i] >= 0 {
                    paint[i] > 0
                } else {
                    inside[i]
                }
            })
            .collect();
        filled.push(false);
        let mut colour: Vec<i64> = (0..amap.count).map(|i| paint[i].max(0)).collect();
        colour.push(-1);
        (filled, colour)
    }

    /// Every line that cuts areas, in beats / keys.
    fn area_lines(&self) -> Vec<Seg> {
        let mut v = self.plan().polys.clone();
        v.extend(self.area_cuts());
        shrink::segments(&v)
    }

    /// The lines with a filled area on one side only (or two colours
    /// meeting), in beats / keys.
    fn area_edges(&self, amap: &AreaMap, borders_only: bool) -> Vec<Seg> {
        let seg = self.area_lines();
        if seg.is_empty() {
            return seg;
        }
        let (filled, colour) = self.area_state(amap);
        let ends_a: Vec<P> = seg.iter().map(|s| [s[0], s[1]]).collect();
        let ends_b: Vec<P> = seg.iter().map(|s| [s[2], s[3]]).collect();
        let a = self.uv_points(&ends_a).unwrap_or_default();
        let b = self.uv_points(&ends_b).unwrap_or_default();
        let k = amap.k;
        let borders = self.flag("borders");
        let idx = |lab: i32| {
            if lab < 0 {
                filled.len() - 1
            } else {
                lab as usize
            }
        };
        // pieces: (line, piece seg, keep)
        let mut pieces: Vec<(usize, Seg, bool)> = Vec::new();
        for i in 0..seg.len() {
            let cells = ((b[i][0] - a[i][0]) * k[0]).hypot((b[i][1] - a[i][1]) * k[1]);
            let n = np_i64((cells / 4.0).ceil()).max(1);
            for j in 0..n {
                let t0 = j as f64 / n as f64;
                let t1 = t0 + 1.0 / n as f64;
                let s = &seg[i];
                let piece = [
                    s[0] + (s[2] - s[0]) * t0,
                    s[1] + (s[3] - s[1]) * t0,
                    s[0] + (s[2] - s[0]) * t1,
                    s[1] + (s[3] - s[1]) * t1,
                ];
                let pa = [
                    a[i][0] + (b[i][0] - a[i][0]) * t0,
                    a[i][1] + (b[i][1] - a[i][1]) * t0,
                ];
                let pb = [
                    a[i][0] + (b[i][0] - a[i][0]) * t1,
                    a[i][1] + (b[i][1] - a[i][1]) * t1,
                ];
                let d = [(pb[0] - pa[0]) * k[0], (pb[1] - pa[1]) * k[1]];
                let ln = d[0].hypot(d[1]);
                let ok = ln > 1e-9;
                let l = if ok { ln } else { 1.0 };
                let nv = [-d[1] / l * 1.5 / k[0], d[0] / l * 1.5 / k[1]];
                let mid = [(pa[0] + pb[0]) / 2.0, (pa[1] + pb[1]) / 2.0];
                let one = idx(amap.at(mid[0] + nv[0], mid[1] + nv[1]));
                let two = idx(amap.at(mid[0] - nv[0], mid[1] - nv[1]));
                let meet = filled[one] && filled[two] && colour[one] != colour[two];
                let differ = if borders_only {
                    meet
                } else {
                    (filled[one] != filled[two]) || (meet && borders)
                };
                pieces.push((i, piece, ok && differ));
            }
        }
        // pieces kept one after another on the same line: one piece again
        let mut out: Vec<Seg> = Vec::new();
        let mut prev: Option<(usize, usize)> = None; // (line, position)
        for (pos, (i, p, keep)) in pieces.iter().enumerate() {
            if !keep {
                continue;
            }
            match prev {
                Some((li, lp)) if li == *i && lp + 1 == pos => {
                    let l = out.last_mut().expect("open");
                    l[2] = p[2];
                    l[3] = p[3];
                }
                _ => out.push(*p),
            }
            prev = Some((*i, pos));
        }
        out
    }

    /// What the areas fill on `SAMPLES` lines across each key, with the
    /// heights, area_edges and the shape's proportion (None: flat box).
    fn area_filled_lines(&self, keys: &[i64]) -> (Vec<f64>, Option<FilledLines>) {
        let ys = shrink::sample_heights(keys, 1.0);
        let Some(amap) = self.shape_areas() else {
            return (ys, None);
        };
        let (filled, _) = self.area_state(amap);
        let lines = self.area_lines();
        let mut out = Vec::with_capacity(ys.len());
        for &y in &ys {
            let mut x: Vec<f64> = lines
                .iter()
                .filter(|s| (s[1] <= y) != (s[3] <= y))
                .map(|s| s[0] + (s[2] - s[0]) * (y - s[1]) / (s[3] - s[1]))
                .collect();
            x.sort_by(|a, b| fcmp(*a, *b));
            if x.len() < 2 {
                out.push(Vec::new());
                continue;
            }
            let m: Vec<P> = x.windows(2).map(|w| [(w[0] + w[1]) / 2.0, y]).collect();
            let uv = self.uv_points(&m).unwrap_or_default();
            let mut row = Vec::new();
            for (w, q) in x.windows(2).zip(&uv) {
                let lab = amap.at(q[0], q[1]);
                let i = if lab < 0 {
                    filled.len() - 1
                } else {
                    lab as usize
                };
                if filled[i] {
                    row.push([w[0], w[1]]);
                }
            }
            out.push(row);
        }
        let edges = self.area_edges(amap, false);
        let mut polys = self.plan().polys.clone();
        polys.extend(self.area_cuts());
        (ys, Some((out, edges, shrink::proportion(&polys))))
    }

    /// What's filled shrunk by g ticks from area_edges, per sample line.
    fn area_inner_lines(&self, g: i64, keys: &[i64]) -> Vec<Vec<[f64; 2]>> {
        let (ys, got) = self.area_filled_lines(keys);
        let Some((on, edges, k)) = got else {
            return vec![Vec::new(); ys.len()];
        };
        let ky: Vec<f64> = ys.iter().map(|y| y * k).collect();
        let near = shrink::near_rows(
            &shrink::scaled(&edges, k),
            &ky,
            g as f64 / f64::from(self.ppq),
        );
        on.iter()
            .zip(&near)
            .map(|(spans, cut)| shrink::merge(shrink::minus(spans, cut)))
            .collect()
    }

    /// `inner_ticks` for coloured areas.
    fn area_inner(&self, g: i64, keys: &[i64]) -> Vec<N3> {
        let per = self.area_inner_lines(g, keys);
        let ppqf = f64::from(self.ppq);
        let mut out = Vec::new();
        for (n, &q) in keys.iter().enumerate() {
            let got: Vec<[f64; 2]> = per[n * SAMPLES..(n + 1) * SAMPLES]
                .iter()
                .flatten()
                .copied()
                .collect();
            let mut row: Vec<N3> = Vec::new();
            for [a, b] in shrink::merge(got) {
                let s = (a * ppqf + 0.5).floor() as i64;
                let e = (b * ppqf + 0.5).floor() as i64;
                if e <= s {
                    continue;
                }
                if let Some(l) = row.last_mut()
                    && s - l[1] < g
                {
                    l[1] = l[1].max(e);
                    continue;
                }
                row.push([s, e, q]);
            }
            out.extend(row);
        }
        out
    }

    /// has_areas shapes: `(start, end, key, colour)` of every stretch filled.
    pub fn area_spans(&self) -> &Vec<[i64; 4]> {
        self.aspans.get_or_init(|| self.find_area_spans())
    }

    fn find_area_spans(&self) -> Vec<[i64; 4]> {
        let Some(amap) = self.shape_areas() else {
            return Vec::new();
        };
        let polys = &self.plan().polys;
        let cuts = self.area_cuts();
        let ps: Vec<f64> = polys
            .iter()
            .chain(cuts.iter())
            .flatten()
            .map(|p| p[1])
            .collect();
        if ps.is_empty() {
            return Vec::new();
        }
        let paint = self.area_paint(amap);
        let mut ed = Edges::default();
        for (i, p) in polys.iter().enumerate() {
            if p.len() > 1 {
                for w in p.windows(2) {
                    if w[0][1] != w[1][1] {
                        ed.push(w[0], w[1], i as i64);
                    }
                }
            }
        }
        for p in &cuts {
            if p.len() > 1 {
                for w in p.windows(2) {
                    if w[0][1] != w[1][1] {
                        ed.push(w[0], w[1], -1);
                    }
                }
            }
        }
        let union = self.flag("union");
        let colour = |pieces: &[Vec<P>]| -> Vec<i64> { self.piece_colours(amap, &paint, pieces) };
        let ppqf = f64::from(self.ppq);
        let lo_p = ps
            .iter()
            .copied()
            .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
        let hi_p = ps
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, |a, b| if b > a { b } else { a });
        let mut out = Vec::new();
        for q in pitch_of(lo_p).max(0)..=pitch_of(hi_p).min(TOP_KEY) {
            for (s, e, g) in row_pieces(&ed, q, union, &colour) {
                let s = (s * ppqf + 0.5).floor() as i64;
                out.push([s, ((e * ppqf + 0.5).floor() as i64).max(s + 1), q, g]);
            }
        }
        out
    }

    /// For each piece (spots in beats / keys): the colour of the area most
    /// of its spots are in (`colour` in find_area_spans).
    fn piece_colours(&self, amap: &AreaMap, paint: &[i64], pieces: &[Vec<P>]) -> Vec<i64> {
        let mut pick = vec![-1i64; pieces.len()];
        for (i, spots) in pieces.iter().enumerate() {
            let uv = self.uv_points(spots).unwrap_or_default();
            let row: Vec<i64> = uv
                .iter()
                .map(|q| i64::from(amap.cell(q[0], q[1], -1)))
                .filter(|v| *v >= 0)
                .collect();
            if !row.is_empty() {
                pick[i] = most_common(&row);
            } else {
                let q = self.uv_points(&spots[..1]).unwrap_or_default();
                pick[i] = i64::from(amap.at(q[0][0], q[0][1]));
            }
        }
        pick.iter()
            .map(|&p| paint[if p < 0 { paint.len() - 1 } else { p as usize }])
            .collect()
    }

    /// "Outline between colours": the lines where two colours meet as notes.
    fn border_lines(&self) -> Vec<N3> {
        let seg = match self.shape_areas() {
            Some(amap) => self.area_edges(amap, true),
            None => Vec::new(),
        };
        let ppqf = f64::from(self.ppq);
        let mut rows = Vec::new();
        for s in &seg {
            let up = s[1] <= s[3];
            let (xa, ya, xb, yb) = if up {
                (s[0], s[1], s[2], s[3])
            } else {
                (s[2], s[3], s[0], s[1])
            };
            let q0 = np_i64((ya + 0.5).floor());
            let q1 = np_i64((yb + 0.5).floor());
            let flat = ya == yb;
            for q in q0..=q1 {
                let lo = npmax(ya, q as f64 - 0.5);
                let hi = npmin(yb, q as f64 + 0.5);
                if !(flat || hi > lo) {
                    continue;
                }
                let dy = if flat { 1.0 } else { yb - ya };
                let x0 = if flat {
                    xa
                } else {
                    xa + (xb - xa) * (lo - ya) / dy
                };
                let x1 = if flat {
                    xb
                } else {
                    xa + (xb - xa) * (hi - ya) / dy
                };
                let st = np_i64((npmin(x0, x1) * ppqf + 0.5).floor());
                let e = np_i64((npmax(x0, x1) * ppqf + 0.5).floor()).max(st + 1);
                rows.push([st, e, q]);
            }
        }
        merged_rows(&rows)
    }

    // ------------------------------------------------------------ outline gate

    /// The smallest outline gate in ticks (0 = off).
    fn edge_gate(&self) -> i64 {
        match self.sh.get("edge").filter(|e| e.truthy()).and_then(fnum) {
            Some(e) => (curves::rne(e * f64::from(self.ppq)) as i64).max(1),
            None => 0,
        }
    }

    /// The inside's stretches, `(start, end, key)`.
    fn inside_rows(&self) -> Vec<N3> {
        if self.strokes.is_empty() {
            return Vec::new();
        }
        if self.has_areas() {
            let s: Vec<N3> = self
                .area_spans()
                .iter()
                .map(|r| [r[0], r[1], r[2]])
                .collect();
            return merged_rows(&s);
        }
        self.inside_spans().clone()
    }

    /// The shape shrunk inward by the outline gate g (`inner_ticks`).
    fn inner_ticks(&self, g: i64, spans: &[N3]) -> &Vec<N3> {
        self.inner.get_or_init(|| {
            let mut keys: Vec<i64> = spans.iter().map(|r| r[2]).collect();
            keys.sort_unstable();
            keys.dedup();
            if keys.is_empty() {
                return Vec::new();
            }
            if self.has_areas() {
                self.area_inner(g, &keys)
            } else {
                let text = self.text().is_some();
                let loops = if text {
                    self.paths()
                } else {
                    &self.plan().polys
                };
                let union = self.flag("union") && !text;
                shrink::inner_rows(
                    loops,
                    union,
                    &keys,
                    g as f64 / f64::from(self.ppq),
                    self.ppq,
                )
            }
        })
    }

    /// Outline notes with the smallest outline gate.
    fn thicker(&self, notes: Vec<N3>, spans: &[N3], g: i64) -> Vec<N3> {
        if g == 0 || spans.is_empty() {
            return notes;
        }
        if self.sideways() {
            return grow_inward(&notes, spans, g);
        }
        with_band(&notes, spans, self.inner_ticks(g, spans))
    }

    // ------------------------------------------------------------ spam

    /// The cleaned Hz bass settings.
    pub fn hz(&self) -> Option<Hz> {
        self.sh
            .get("hz")
            .filter(|h| h.truthy())
            .and_then(|h| Hz::clean(Some(h)))
    }

    /// The spam gate: whole ticks; for Hz bass the exact one (or the placed
    /// tones' repeats).
    pub fn spam_gate(&self) -> &Gate {
        self.gate.get_or_init(|| {
            let g = super::getf(self.sh, "gate", 0.0625);
            let ppq = f64::from(self.ppq);
            if let Some(hz) = self.hz() {
                if !hz.tones.is_empty() {
                    let pts = super::shape_pts(self.sh);
                    return match hzbass::squares(&hz, &pts, self.ppq) {
                        hzbass::Squares::Plain(sq) => Gate::Grid(sq),
                        hzbass::Squares::Keys(k) => Gate::Keys(Box::new(k)),
                    };
                }
                let gate = (g * ppq).max(1.0);
                let limit = hz.threshold(None);
                let whole = hz.fixed || limit.is_some_and(|l| hzbass::off_cents(gate) <= l + 1e-9);
                return Gate::Exact(if whole { (gate + 0.5).floor() } else { gate });
            }
            Gate::Ticks(((g * ppq + 0.5).floor() as i64).max(1))
        })
    }

    fn chop_outline(&self, notes: &[N3]) -> Vec<N3> {
        chop(self.sh, notes, self.spam_gate())
    }

    /// `{group: strokes}` when the strokes came from different shapes or
    /// have outline colours of their own.
    fn stroke_groups(&self) -> Option<Vec<Vec<usize>>> {
        let mut groups: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
        for (k, st) in self.strokes.iter().enumerate() {
            let src = st.get("src").and_then(py_int).unwrap_or(-1);
            groups.entry((src, colour_of(st))).or_default().push(k);
        }
        (groups.len() > 1).then(|| groups.into_values().collect())
    }

    fn coloured_strokes(&self) -> bool {
        self.text().is_none()
            && !self.sh.has("notes")
            && self.strokes.iter().any(|st| colour_of(st) != 0)
    }

    /// The outline's notes (spam: chopped) and their stroke groups.
    fn outline_groups(&self, spam: bool) -> (Vec<N3>, Option<Vec<i64>>) {
        let groups = self.stroke_groups();
        let g = self.edge_gate();
        let spans = if g != 0 {
            self.inside_rows()
        } else {
            Vec::new()
        };
        let Some(groups) = groups else {
            let notes = self.thicker(self.outline_notes(None), &spans, g);
            return (
                if spam {
                    self.chop_outline(&notes)
                } else {
                    notes
                },
                None,
            );
        };
        let (mut parts, mut ids) = (Vec::new(), Vec::new());
        for (n, strokes) in groups.iter().enumerate() {
            let mut notes = self.outline_notes(Some(strokes));
            if n == 0 || self.sideways() {
                notes = self.thicker(notes, &spans, g);
            }
            if spam {
                notes = self.chop_outline(&notes);
            }
            ids.extend(std::iter::repeat_n(n as i64, notes.len()));
            parts.extend(notes);
        }
        (parts, Some(ids))
    }

    /// Outline notes of parts too flat to fill.
    fn flat_notes(&self) -> Vec<N3> {
        if self.text().is_some() {
            return Vec::new();
        }
        let flat = &self.plan().flat;
        if flat.is_empty() {
            Vec::new()
        } else {
            paths_outline(flat, self.ppq)
        }
    }

    /// Outline-only strokes' notes and their outline colours.
    fn edge_lines(&self) -> (Vec<N3>, Vec<i64>) {
        if self.text().is_some() {
            return (Vec::new(), Vec::new());
        }
        let mut by: BTreeMap<i64, Vec<Vec<P>>> = BTreeMap::new();
        for (st, path) in self.strokes.iter().zip(self.paths()) {
            if role_of(st) == Some("edge") {
                by.entry(colour_of(st)).or_default().push(path.clone());
            }
        }
        let (mut notes, mut cols) = (Vec::new(), Vec::new());
        for (c, ps) in by {
            let n = paths_outline(&join_paths(ps), self.ppq);
            cols.extend(std::iter::repeat_n(c, n.len()));
            notes.extend(n);
        }
        (notes, cols)
    }

    /// Notes, and which group each belongs to (None = all one), with
    /// "Colours" applied (`custom_notes_groups`).
    pub fn notes_groups(&self) -> (Vec<N3>, Option<Vec<i64>>) {
        let (notes, groups) = self.raw_groups();
        if !super::cycling(self.sh) || notes.is_empty() {
            return (notes, groups);
        }
        let n = self
            .sh
            .get("cycle")
            .and_then(|c| c.get("n"))
            .and_then(inum)
            .unwrap_or(2);
        let turn = after::cycle_turns(self.sh, &notes, self.ppq);
        let Some(groups) = groups else {
            return (notes, Some(turn));
        };
        let apart = self.flag("apart") && matches!(self.fill(), "fill" | "spam");
        let g = groups
            .iter()
            .zip(&turn)
            .map(|(&g, &t)| {
                if !apart {
                    g * n + t
                } else if g == 0 {
                    0
                } else {
                    1 + (g - 1) * n + t
                }
            })
            .collect();
        (notes, Some(g))
    }

    fn raw_groups(&self) -> (Vec<N3>, Option<Vec<i64>>) {
        if self.sh.has("notes") {
            let b = block_notes(self.sh, self.ppq);
            return (b.iter().map(|r| [r[0], r[1], r[2]]).collect(), None);
        }
        let fill = self.fill();
        if fill == "outline_spam" {
            return self.outline_groups(true);
        }
        if fill == "empty" || self.strokes.is_empty() {
            return self.outline_groups(false);
        }
        let (spans, area, rows): (Vec<N3>, Option<Vec<i64>>, Vec<N3>) = if self.has_areas() {
            let got = self.area_spans();
            let spans: Vec<N3> = got.iter().map(|r| [r[0], r[1], r[2]]).collect();
            let rows = merged_rows(&spans);
            (spans, Some(got.iter().map(|r| r[3]).collect()), rows)
        } else {
            let s = self.inside_spans().clone();
            (s.clone(), None, s)
        };
        let flat = self.flat_notes();
        let (lines, lc) = self.edge_lines();
        let coloured = area.is_some() || lc.iter().any(|c| *c != 0);
        let apart = self.flag("apart");
        let g = if apart { self.edge_gate() } else { 0 };
        let borders = self.flag("borders");
        let lc_ids = |lc: &[i64]| -> Vec<i64> {
            lc.iter().map(|&c| if c > 0 { 1 + c } else { 0 }).collect()
        };
        if fill == "fill" {
            let notes: Vec<N3> = spans.iter().chain(&flat).copied().collect();
            if apart {
                let mut outline = edge_parts(&notes);
                if g != 0 {
                    let t = self.thicker(outline, &rows, g);
                    outline = by_key_rows(&t);
                }
                if area.is_some() && borders {
                    let mut all = outline.clone();
                    all.extend(border_parts(&spans, &self.border_lines()));
                    outline = merged_rows(&all);
                }
                let (inside, ids): (Vec<N3>, Vec<i64>) = match &area {
                    None => {
                        let inside = cut_out(&notes, &outline);
                        let n = inside.len();
                        (inside, vec![1; n])
                    }
                    Some(area) => {
                        let mut ks: Vec<i64> = area.clone();
                        ks.sort_unstable();
                        ks.dedup();
                        let mut inside = Vec::new();
                        let mut ids = Vec::new();
                        for k in ks {
                            let sel: Vec<N3> = spans
                                .iter()
                                .zip(area)
                                .filter(|(_, a)| **a == k)
                                .map(|(s, _)| *s)
                                .collect();
                            let p = cut_out(&sel, &outline);
                            ids.extend(std::iter::repeat_n(1 + k, p.len()));
                            inside.extend(p);
                        }
                        let p = cut_out(&flat, &outline);
                        ids.extend(std::iter::repeat_n(1, p.len()));
                        inside.extend(p);
                        (inside, ids)
                    }
                };
                let mut gr = vec![0i64; outline.len()];
                gr.extend(lc_ids(&lc));
                gr.extend(ids);
                let mut all = outline;
                all.extend(lines);
                all.extend(inside);
                return (all, Some(gr));
            }
            let mut all = notes;
            all.extend(lines);
            if !coloured {
                return (all, None);
            }
            let mut gr = area.unwrap_or_else(|| vec![0; spans.len()]);
            gr.extend(std::iter::repeat_n(0, flat.len()));
            gr.extend(lc);
            return (all, Some(gr));
        }
        let gate = self.spam_gate();
        let main = chop(self.sh, &rows, gate);
        let mut notes = main.clone();
        notes.extend(self.chop_outline(&flat));
        let ids: Option<Vec<i64>> = area.as_ref().map(|area| {
            let mut v = colour_at(&main, &spans, area);
            v.extend(std::iter::repeat_n(0, notes.len() - main.len()));
            v
        });
        let lc: Vec<i64> = if lines.is_empty() {
            lc
        } else {
            let c = chop_counts(self.sh, &lines, gate);
            lc.iter()
                .zip(&c)
                .flat_map(|(&l, &n)| std::iter::repeat_n(l, n.max(0) as usize))
                .collect()
        };
        let lines = self.chop_outline(&lines);
        if apart {
            let mut edge = on_edge(&notes);
            if g != 0 {
                if self.sideways() {
                    let near = near_edge(&notes, &rows, g);
                    for (e, n) in edge.iter_mut().zip(near) {
                        *e |= n;
                    }
                } else {
                    let cov = covered(&notes, self.inner_ticks(g, &rows));
                    for (e, c) in edge.iter_mut().zip(cov) {
                        *e |= !c;
                    }
                }
            }
            if area.is_some() && borders {
                let ce = colour_edges(&main, &self.border_lines());
                for (e, c) in edge.iter_mut().zip(ce) {
                    *e |= c;
                }
            }
            let mut gr: Vec<i64> = edge
                .iter()
                .enumerate()
                .map(|(i, &e)| {
                    if e {
                        0
                    } else {
                        1 + ids.as_ref().map_or(0, |v| v[i])
                    }
                })
                .collect();
            gr.extend(lc_ids(&lc));
            let mut all = notes;
            all.extend(lines);
            return (all, Some(gr));
        }
        let mut all = notes;
        let n_notes = all.len();
        all.extend(lines);
        if !coloured {
            return (all, None);
        }
        let mut gr = ids.unwrap_or_else(|| vec![0; n_notes]);
        gr.extend(lc);
        (all, Some(gr))
    }

    /// About how many notes the shape makes, worked out without making the
    /// spam (preflight).
    pub fn estimate(&self) -> u64 {
        if self.sh.has("notes") {
            return gets(self.sh, "notes")
                .and_then(unpack_notes)
                .map_or(0, |r| r.len() as u64);
        }
        let sum = |v: Vec<i64>| v.iter().map(|x| (*x).max(0) as u64).sum::<u64>();
        let fill = self.fill();
        if fill == "outline_spam" {
            let (n, _) = self.outline_groups(false);
            return sum(chop_counts(self.sh, &n, self.spam_gate()));
        }
        if fill == "empty" || self.strokes.is_empty() {
            return self.outline_groups(false).0.len() as u64;
        }
        // (spam chops whole filled stretches, whatever their colours)
        let rows = if self.has_areas() && fill == "fill" {
            self.area_spans()
                .iter()
                .map(|r| [r[0], r[1], r[2]])
                .collect()
        } else {
            self.inside_rows()
        };
        let flat = self.flat_notes();
        let (lines, _) = self.edge_lines();
        if fill == "fill" {
            let n = (rows.len() + flat.len() + lines.len()) as u64;
            return if self.flag("apart") {
                n.saturating_mul(3)
            } else {
                n
            };
        }
        let g = self.spam_gate();
        sum(chop_counts(self.sh, &rows, g))
            + sum(chop_counts(self.sh, &flat, g))
            + sum(chop_counts(self.sh, &lines, g))
    }

    /// Whether the shape's tracks must get channels of their own.
    pub fn tracks_apart(&self) -> bool {
        self.sh.has("notes")
            || (self.flag("apart") && matches!(self.fill(), "fill" | "spam"))
            || super::cycling(self.sh)
            || (self.has_areas() && self.areas().iter().any(|a| a.2 != 0))
            || self.coloured_strokes()
    }
}

/// The value most often in `row`, ties going to the first in CPython's
/// `set(row)` order (`max(set(row), key=row.count)`).
fn most_common(row: &[i64]) -> i64 {
    let order = py_set_order(row);
    let mut best = order[0];
    let mut most = row.iter().filter(|v| **v == best).count();
    for &v in &order[1..] {
        let c = row.iter().filter(|x| **x == v).count();
        if c > most {
            best = v;
            most = c;
        }
    }
    best
}

/// The iteration order of CPython's `set(items)` for non-negative ints
/// (hash = value): open addressing with linear probes and perturbation,
/// resized at 3/5 full (setobject.c, Python 3.13).
pub fn py_set_order(items: &[i64]) -> Vec<i64> {
    const LINEAR_PROBES: usize = 9;
    const PERTURB_SHIFT: u32 = 5;
    fn insert_clean(table: &mut [Option<i64>], mask: usize, key: i64) {
        let mut perturb = key as u64;
        let mut i = (key as usize) & mask;
        loop {
            if table[i].is_none() {
                table[i] = Some(key);
                return;
            }
            if i + LINEAR_PROBES <= mask {
                for j in 1..=LINEAR_PROBES {
                    if table[i + j].is_none() {
                        table[i + j] = Some(key);
                        return;
                    }
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i
                .wrapping_mul(5)
                .wrapping_add(1)
                .wrapping_add(perturb as usize))
                & mask;
        }
    }
    let mut mask = 7usize;
    let mut table: Vec<Option<i64>> = vec![None; 8];
    let (mut fill, mut used) = (0usize, 0usize);
    for &key in items {
        let mut perturb = key as u64;
        let mut i = (key as usize) & mask;
        let slot;
        'probe: loop {
            let mut probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            let mut e = i;
            loop {
                match table[e] {
                    None => {
                        slot = Some(e);
                        break 'probe;
                    }
                    Some(k) if k == key => {
                        slot = None;
                        break 'probe;
                    }
                    _ => {}
                }
                if probes == 0 {
                    break;
                }
                probes -= 1;
                e += 1;
            }
            perturb >>= PERTURB_SHIFT;
            i = (i
                .wrapping_mul(5)
                .wrapping_add(1)
                .wrapping_add(perturb as usize))
                & mask;
        }
        let Some(e) = slot else { continue };
        table[e] = Some(key);
        fill += 1;
        used += 1;
        if fill * 5 >= mask * 3 {
            let minused = if used > 50_000 { used * 2 } else { used * 4 };
            let mut newsize = 8usize;
            while newsize <= minused {
                newsize <<= 1;
            }
            let old = std::mem::replace(&mut table, vec![None; newsize]);
            mask = newsize - 1;
            for k in old.into_iter().flatten() {
                insert_clean(&mut table, mask, k);
            }
        }
    }
    table.into_iter().flatten().collect()
}

/// `row_pieces`: every line cuts row q into pieces, each filled as normal
/// or as `colour` says; `[(start, end, colour)]` in beats, merged per colour.
fn row_pieces(
    ed: &Edges,
    q: i64,
    union: bool,
    colour: &dyn Fn(&[Vec<P>]) -> Vec<i64>,
) -> Vec<(f64, f64, i64)> {
    let (lo, hi) = (q as f64 - 0.5, q as f64 + 0.5);
    let here = rows_here(ed, lo, hi);
    if here.is_empty() {
        return Vec::new();
    }
    let sl = slices(ed, &here, lo, hi);
    let lv = &sl.levels;
    // sorted by (gap, x at middle), stable
    let mut ord: Vec<usize> = (0..sl.pairs.len()).collect();
    let xm_all: Vec<f64> = sl
        .pairs
        .iter()
        .map(|&(i, g)| x_at(ed, i, (lv[g] + lv[g + 1]) / 2.0))
        .collect();
    ord.sort_by(|&a, &b| {
        sl.pairs[a]
            .1
            .cmp(&sl.pairs[b].1)
            .then(fcmp(xm_all[a], xm_all[b]))
    });
    let m = ord.len();
    let gap: Vec<usize> = ord.iter().map(|&o| sl.pairs[o].1).collect();
    let eid: Vec<usize> = ord.iter().map(|&o| sl.pairs[o].0).collect();
    let lid: Vec<i64> = eid.iter().map(|&i| ed.lid[i]).collect();
    let xm: Vec<f64> = ord.iter().map(|&o| xm_all[o]).collect();
    let y0: Vec<f64> = gap.iter().map(|&g| lv[g]).collect();
    let y1: Vec<f64> = gap.iter().map(|&g| lv[g + 1]).collect();
    let xl: Vec<f64> = (0..m)
        .map(|j| npmin(x_at(ed, eid[j], y0[j]), x_at(ed, eid[j], y1[j])))
        .collect();
    let xr: Vec<f64> = (0..m)
        .map(|j| npmax(x_at(ed, eid[j], y0[j]), x_at(ed, eid[j], y1[j])))
        .collect();
    let start: Vec<bool> = (0..m).map(|j| j == 0 || gap[j] != gap[j - 1]).collect();
    let fill: Vec<bool> = lid.iter().map(|l| *l >= 0).collect();
    let step: Vec<i64> = if union {
        // each loop on its own: its 1st crossing in the slice goes in, the 2nd out ...
        let mut o2: Vec<usize> = (0..m).collect();
        o2.sort_by(|&a, &b| {
            gap[a]
                .cmp(&gap[b])
                .then(lid[a].cmp(&lid[b]))
                .then(a.cmp(&b))
        });
        let mut step = vec![0i64; m];
        let mut rank = 0usize;
        for (p, &o) in o2.iter().enumerate() {
            if p == 0 || gap[o2[p - 1]] != gap[o] || lid[o2[p - 1]] != lid[o] {
                rank = 0;
            }
            step[o] = if rank.is_multiple_of(2) { 1 } else { -1 };
            rank += 1;
        }
        for (s, f) in step.iter_mut().zip(&fill) {
            if !f {
                *s = 0;
            }
        }
        step
    } else {
        fill.iter().map(|&f| i64::from(f)).collect()
    };
    let mut inside = vec![false; m];
    let mut depth = 0i64;
    for j in 0..m {
        if start[j] {
            depth = 0;
        }
        depth += step[j];
        inside[j] = if union {
            depth > 0
        } else {
            depth.rem_euclid(2) == 1
        };
    }
    // pieces: from crossing j to j + 1 in the same slice
    let pi: Vec<usize> = (0..m).filter(|&j| j + 1 < m && !start[j + 1]).collect();
    if pi.is_empty() {
        return Vec::new();
    }
    let fys = [0.5, 0.2, 0.8];
    let pieces: Vec<Vec<P>> = pi
        .iter()
        .map(|&j| {
            let mut spots = Vec::with_capacity(9);
            for fy in fys {
                let y = y0[j] + (y1[j] - y0[j]) * fy;
                let (xa, xb) = (x_at(ed, eid[j], y), x_at(ed, eid[j + 1], y));
                for fx in fys {
                    spots.push([xa + (xb - xa) * fx, y]);
                }
            }
            spots
        })
        .collect();
    // (the next crossing's y in its own slice row: y[order][pi] uses the piece's
    // slice, the same for j and j + 1)
    let c = colour(&pieces);
    let np_ = pi.len();
    let filled: Vec<bool> = (0..np_)
        .map(|p| if c[p] >= 0 { c[p] > 0 } else { inside[pi[p]] })
        .collect();
    let group: Vec<i64> = c.iter().map(|&v| v.max(0)).collect();
    let joined: Vec<bool> = (0..np_).map(|p| p > 0 && pi[p] == pi[p - 1] + 1).collect();
    let mut lo_v = Vec::new();
    let mut hi_v = Vec::new();
    let mut k_v = Vec::new();
    let mut tall = Vec::new();
    for p in 0..np_ {
        if !filled[p] {
            continue;
        }
        let left_full = p > 0 && joined[p] && filled[p - 1];
        let right_full = p + 1 < np_ && joined[p + 1] && filled[p + 1];
        let j = pi[p];
        lo_v.push(if left_full { xm[j] } else { xl[j] });
        hi_v.push(if right_full { xm[j + 1] } else { xr[j + 1] });
        k_v.push(group[p]);
        tall.push(y1[j] - y0[j]);
    }
    let mut names: Vec<i64> = k_v.clone();
    names.sort_unstable();
    names.dedup();
    if names.len() <= 1 {
        if k_v.is_empty() {
            return Vec::new();
        }
        return merge_spans(&lo_v, &hi_v)
            .into_iter()
            .map(|[s, e]| (s, e, k_v[0]))
            .collect();
    }
    // colours overlapping in the row: each bit of time goes to the colour
    // filling most of the row's height there
    let at = unique_f(lo_v.iter().chain(&hi_v).copied().collect());
    let nb = at.len().saturating_sub(1);
    let mut best = vec![0i64; nb];
    let mut has = vec![false; nb];
    let mids: Vec<f64> = (0..nb).map(|i| (at[i] + at[i + 1]) / 2.0).collect();
    for i in 0..nb {
        let mm = mids[i];
        let mut height = vec![0.0f64; names.len()];
        for (j, nm) in names.iter().enumerate() {
            // (cover * tall).sum(1): NumPy's pairwise sum over the pieces
            let vals: Vec<f64> = (0..lo_v.len())
                .filter(|&p| k_v[p] == *nm)
                .map(|p| {
                    if lo_v[p] <= mm && hi_v[p] >= mm {
                        tall[p]
                    } else {
                        0.0
                    }
                })
                .collect();
            height[j] = super::np::pw_sum(&vals);
            if lo_v.iter().zip(&hi_v).any(|(l, h)| *l <= mm && *h >= mm) {
                has[i] = true;
            }
        }
        let mut arg = 0;
        for j in 1..names.len() {
            if height[j] > height[arg] {
                arg = j;
            }
        }
        best[i] = names[arg];
    }
    let (cx, cfill, cgroup) = centre_colours(ed, &here, q, union, colour);
    if cx.len() >= 2 {
        for i in 0..nb {
            let j =
                cx.partition_point(|x| fcmp(*x, mids[i]) == std::cmp::Ordering::Less) as i64 - 1;
            if j >= 0 && (j as usize) < cx.len() - 1 && cfill[j as usize] {
                best[i] = cgroup[j as usize];
            }
        }
    }
    let mut out: Vec<(f64, f64, i64)> = Vec::new();
    for i in 0..nb {
        if !has[i] {
            continue;
        }
        let (s, e, c) = (at[i], at[i + 1], best[i]);
        if let Some(l) = out.last_mut()
            && l.2 == c
            && l.1 >= s
        {
            l.1 = e;
            continue;
        }
        out.push((s, e, c));
    }
    out
}

/// Where the lines cross the middle of row q (x in order), and each piece
/// between two of them: filled and its colour.
fn centre_colours(
    ed: &Edges,
    here: &[usize],
    q: i64,
    union: bool,
    colour: &dyn Fn(&[Vec<P>]) -> Vec<i64>,
) -> (Vec<f64>, Vec<bool>, Vec<i64>) {
    let qf = q as f64;
    let mut xs: Vec<(f64, i64)> = here
        .iter()
        .filter(|&&i| npmin(ed.ya[i], ed.yb[i]) <= qf && npmax(ed.ya[i], ed.yb[i]) > qf)
        .map(|&i| {
            (
                ed.xa[i] + (ed.xb[i] - ed.xa[i]) * (qf - ed.ya[i]) / (ed.yb[i] - ed.ya[i]),
                ed.lid[i],
            )
        })
        .collect();
    xs.sort_by(|a, b| fcmp(a.0, b.0));
    let x: Vec<f64> = xs.iter().map(|v| v.0).collect();
    let lid: Vec<i64> = xs.iter().map(|v| v.1).collect();
    if x.len() < 2 {
        return (x, Vec::new(), Vec::new());
    }
    let inside: Vec<bool> = if union {
        let mut odd: Vec<i64> = Vec::new();
        lid[..lid.len() - 1]
            .iter()
            .map(|&lp| {
                if lp >= 0 {
                    if let Some(p) = odd.iter().position(|v| *v == lp) {
                        odd.remove(p);
                    } else {
                        odd.push(lp);
                    }
                }
                !odd.is_empty()
            })
            .collect()
    } else {
        let mut c = 0i64;
        lid[..lid.len() - 1]
            .iter()
            .map(|&lp| {
                c += i64::from(lp >= 0);
                c % 2 == 1
            })
            .collect()
    };
    let pieces: Vec<Vec<P>> = x
        .windows(2)
        .map(|w| {
            [0.5, 0.2, 0.8]
                .iter()
                .map(|f| [w[0] + (w[1] - w[0]) * f, qf])
                .collect()
        })
        .collect();
    let paint = colour(&pieces);
    let filled = paint
        .iter()
        .zip(&inside)
        .map(|(&p, &i)| if p >= 0 { p > 0 } else { i })
        .collect();
    let group = paint.iter().map(|&p| p.max(0)).collect();
    (x, filled, group)
}

// ================================================================ stretches

/// `(start, end, key)` notes → per key the stretches they cover:
/// `(key, start, end)` sorted by key and start, not overlapping.
pub fn merged_by_key(notes: &[N3]) -> Vec<(i64, i64, i64)> {
    let mut a: Vec<N3> = notes.to_vec();
    a.sort_by(|x, y| x[2].cmp(&y[2]).then(x[0].cmp(&y[0])));
    let mut out: Vec<(i64, i64, i64)> = Vec::new();
    let mut run = i64::MIN;
    for (i, r) in a.iter().enumerate() {
        let new = i == 0 || r[2] != a[i - 1][2] || r[0] > run;
        if i == 0 || r[2] != a[i - 1][2] {
            run = r[1];
        } else {
            run = run.max(r[1]);
        }
        if new {
            out.push((r[2], r[0], r[1]));
        } else {
            let l = out.last_mut().expect("open");
            l.2 = l.2.max(r[1]);
        }
    }
    out
}

fn by_key_rows(notes: &[N3]) -> Vec<N3> {
    merged_by_key(notes)
        .into_iter()
        .map(|(k, s, e)| [s, e, k])
        .collect()
}

/// Stretches merged per key where they touch (whatever their colour).
pub fn merged_rows(spans: &[N3]) -> Vec<N3> {
    by_key_rows(spans)
}

/// Which notes lie wholly inside what the others cover on the same key.
pub fn covered(notes: &[N3], others: &[N3]) -> Vec<bool> {
    let m = merged_by_key(others);
    if m.is_empty() || notes.is_empty() {
        return vec![false; notes.len()];
    }
    let big: i64 = 1 << 40;
    let keys: Vec<i64> = m
        .iter()
        .map(|r| r.0.wrapping_mul(big).wrapping_add(r.1))
        .collect();
    notes
        .iter()
        .map(|n| {
            let v = n[2].wrapping_mul(big).wrapping_add(n[0]);
            let i = keys.partition_point(|k| *k <= v) as i64 - 1;
            let ok = i >= 0;
            let i = i.max(0) as usize;
            ok && m[i].0 == n[2] && m[i].2 >= n[1]
        })
        .collect()
}

fn shifted(notes: &[N3], keys: i64) -> Vec<N3> {
    notes.iter().map(|r| [r[0], r[1], r[2] + keys]).collect()
}

/// Which notes are on the edge of the area they fill.
pub fn on_edge(notes: &[N3]) -> Vec<bool> {
    let before: Vec<N3> = notes.iter().map(|r| [r[0] - 1, r[0], r[2]]).collect();
    let after_: Vec<N3> = notes.iter().map(|r| [r[1], r[1] + 1, r[2]]).collect();
    let a = covered(notes, &shifted(notes, -1));
    let b = covered(notes, &shifted(notes, 1));
    let c = covered(&before, notes);
    let d = covered(&after_, notes);
    (0..notes.len())
        .map(|i| !(a[i] && b[i] && c[i] && d[i]))
        .collect()
}

/// Fill "Outline": the parts of the long notes on the edge of the area.
pub fn edge_parts(notes: &[N3]) -> Vec<N3> {
    let m = merged_by_key(notes);
    let mut parts = cut_out(notes, &shifted(notes, -1));
    parts.extend(cut_out(notes, &shifted(notes, 1)));
    parts.extend(m.iter().map(|&(k, s, _)| [s, s + 1, k]));
    parts.extend(m.iter().map(|&(k, _, e)| [e - 1, e, k]));
    by_key_rows(&parts)
}

/// Stretches with the time the others cover on the same key taken out.
pub fn cut_out(spans: &[N3], others: &[N3]) -> Vec<N3> {
    let m = merged_by_key(others);
    let mut by_key: BTreeMap<i64, Vec<(i64, i64)>> = BTreeMap::new();
    for (k, s, e) in m {
        by_key.entry(k).or_default().push((s, e));
    }
    let mut out = Vec::new();
    for &[s0, e, k] in spans {
        let mut s = s0;
        if let Some(list) = by_key.get(&k) {
            for &(os, oe) in list {
                if oe <= s || os >= e {
                    continue;
                }
                if os > s {
                    out.push([s, os, k]);
                }
                s = s.max(oe);
                if s >= e {
                    break;
                }
            }
        }
        if s < e {
            out.push([s, e, k]);
        }
    }
    out
}

/// For each note, the inside stretch on its key holding its middle.
fn span_of(notes: &[N3], spans: &[N3]) -> Vec<(i64, i64, bool)> {
    if notes.is_empty() || spans.is_empty() {
        return vec![(0, 0, false); notes.len()];
    }
    let lo = notes
        .iter()
        .map(|r| r[0])
        .chain(spans.iter().map(|r| r[0]))
        .min()
        .expect("some");
    let hi = notes
        .iter()
        .map(|r| r[1])
        .chain(spans.iter().map(|r| r[1]))
        .max()
        .expect("some");
    let big = hi - lo + 2;
    let mut sp = spans.to_vec();
    sp.sort_by(|a, b| a[2].cmp(&b[2]).then(a[0].cmp(&b[0])));
    let keys: Vec<i64> = sp
        .iter()
        .map(|r| r[2].wrapping_mul(big).wrapping_add(r[0] - lo))
        .collect();
    notes
        .iter()
        .map(|n| {
            let mid = (n[0] + n[1]).div_euclid(2);
            let v = n[2].wrapping_mul(big).wrapping_add(mid - lo);
            let i = keys.partition_point(|k| *k <= v) as i64 - 1;
            let found = i >= 0;
            let i = i.max(0) as usize;
            let found = found && sp[i][2] == n[2] && mid <= sp[i][1];
            (sp[i][0], sp[i][1], found)
        })
        .collect()
}

/// Outline notes shorter than g grown into the inside (Grow sideways).
fn grow_inward(notes: &[N3], spans: &[N3], g: i64) -> Vec<N3> {
    if g == 0 || notes.is_empty() {
        return notes.to_vec();
    }
    span_of(notes, spans)
        .iter()
        .zip(notes)
        .map(|(&(a, b, found), n)| {
            let (mut s, mut e) = (n[0], n[1]);
            let m = (a + b).div_euclid(2);
            let left = found && s + e <= a + b;
            let right = found && !left;
            if left {
                e = e.max((s + g).min(m));
            }
            if right {
                s = s.min((e - g).max(m));
            }
            [s, e, n[2]]
        })
        .collect()
}

fn near_edge(notes: &[N3], spans: &[N3], g: i64) -> Vec<bool> {
    span_of(notes, spans)
        .iter()
        .zip(notes)
        .map(|(&(a, b, found), n)| found && (n[0] - a < g || b - n[1] < g))
        .collect()
}

fn with_band(notes: &[N3], spans: &[N3], inner: &[N3]) -> Vec<N3> {
    let mut all = notes.to_vec();
    all.extend(cut_out(spans, inner));
    by_key_rows(&all)
}

fn border_parts(spans: &[N3], lines: &[N3]) -> Vec<N3> {
    cut_out(spans, &cut_out(spans, lines))
}

fn colour_edges(notes: &[N3], lines: &[N3]) -> Vec<bool> {
    covered(notes, &cut_out(notes, lines))
        .into_iter()
        .map(|c| !c)
        .collect()
}

/// For each note, the colour of the stretch on its key holding its middle
/// (none: the nearest one before it on its key, else 0).
fn colour_at(notes: &[N3], spans: &[N3], colours: &[i64]) -> Vec<i64> {
    if notes.is_empty() || spans.is_empty() {
        return vec![0; notes.len()];
    }
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&a, &b| {
        spans[a][2]
            .cmp(&spans[b][2])
            .then(spans[a][0].cmp(&spans[b][0]))
    });
    let big: i64 = 1 << 40;
    let keys: Vec<i64> = order
        .iter()
        .map(|&o| spans[o][2].wrapping_mul(big).wrapping_add(spans[o][0]))
        .collect();
    notes
        .iter()
        .map(|n| {
            let mid = (n[0] + n[1]).div_euclid(2);
            let v = n[2].wrapping_mul(big).wrapping_add(mid);
            let i = keys.partition_point(|k| *k <= v) as i64 - 1;
            let ok = i >= 0;
            let o = order[i.max(0) as usize];
            if ok && spans[o][2] == n[2] {
                colours[o]
            } else {
                0
            }
        })
        .collect()
}

// ================================================================ chop

/// The spam gate: whole ticks, or for Hz bass the exact gate (`chop_even`)
/// or its placed repeats (`chop_grid`).
#[derive(Clone, Debug)]
pub enum Gate {
    Ticks(i64),
    Exact(f64),
    Grid(Vec<[i64; 2]>),
    /// Hz bass with effects: every key its own repeats
    Keys(Box<KeyGrid>),
}

/// How many notes `chop` makes of each stretch.
pub fn chop_counts(sh: &Value, st: &[N3], g: &Gate) -> Vec<i64> {
    chop_core(sh, st, g, true).1
}

/// Stretches filled with back-to-back notes of gate g (same order).
pub fn chop(sh: &Value, st: &[N3], g: &Gate) -> Vec<N3> {
    chop_core(sh, st, g, false).0
}

fn chop_core(sh: &Value, st: &[N3], g: &Gate, count: bool) -> (Vec<N3>, Vec<i64>) {
    let mut out = Vec::new();
    let mut counts = Vec::with_capacity(st.len());
    match g {
        Gate::Exact(g) => {
            let g = *g;
            for &[s0, e0, q] in st {
                let lo = np_i64((s0 as f64 / g - 0.5).ceil());
                let n = np_i64((e0 as f64 / g - 0.5).ceil()) - lo;
                let short = n <= 0;
                let n = if short { 1 } else { n };
                counts.push(n);
                if count {
                    continue;
                }
                for k in lo..lo + n {
                    if short {
                        out.push([s0, e0, q]);
                    } else {
                        out.push([
                            np_i64((k as f64 * g + 0.5).floor()),
                            np_i64(((k + 1) as f64 * g + 0.5).floor()),
                            q,
                        ]);
                    }
                }
            }
        }
        Gate::Grid(sq) => {
            let mids: Vec<i64> = sq.iter().map(|s| s[0] + s[1]).collect();
            for &[s0, e0, q] in st {
                chop_grid(sq, &mids, s0, e0, q, count, &mut out, &mut counts);
            }
        }
        Gate::Keys(grid) => {
            let mut per: BTreeMap<i64, (Vec<[i64; 2]>, Vec<i64>)> = BTreeMap::new();
            for &[s0, e0, q] in st {
                let (sq, mids) = per.entry(q).or_insert_with(|| {
                    let sq = grid.squares(q);
                    let mids = sq.iter().map(|s| s[0] + s[1]).collect();
                    (sq, mids)
                });
                chop_grid(sq, mids, s0, e0, q, count, &mut out, &mut counts);
            }
        }
        Gate::Ticks(g) => {
            let g = *g;
            let ends = gets(sh, "ends").unwrap_or("drop");
            let align = gets(sh, "align").unwrap_or("auto");
            for &[s0, e0, q] in st {
                let size = e0 - s0;
                if matches!(ends, "round" | "stretch") {
                    let n = (2 * size + g).div_euclid(2 * g).max(1);
                    if ends == "stretch" {
                        counts.push(n);
                        if !count {
                            for k in 0..n {
                                out.push([
                                    s0 + (size * k).div_euclid(n),
                                    s0 + (size * (k + 1)).div_euclid(n),
                                    q,
                                ]);
                            }
                        }
                        continue;
                    }
                    let (first, n) = if align == "aligned" {
                        let lo = -(g - 2 * s0).div_euclid(2 * g);
                        let n = (2 * e0 + g).div_euclid(2 * g) - lo;
                        if n <= 0 {
                            ((s0 + e0).div_euclid(2).div_euclid(g) * g, 1)
                        } else {
                            (lo * g, n)
                        }
                    } else if align == "centred" {
                        (s0 + (size - n * g).div_euclid(2), n)
                    } else {
                        (s0, n)
                    };
                    counts.push(n);
                    if !count {
                        for k in 0..n {
                            let a = first + k * g;
                            out.push([a, a + g, q]);
                        }
                    }
                    continue;
                }
                let s = if align == "aligned" {
                    -(-s0).div_euclid(g) * g
                } else if align == "centred" {
                    s0 + size.rem_euclid(g).div_euclid(2)
                } else {
                    s0
                };
                let n = (e0 - s).div_euclid(g).max(0);
                if ends == "keep" {
                    let head_end = s.min(e0);
                    let head = head_end > s0;
                    let tail = (if s < e0 { s + n * g } else { e0 }) < e0;
                    let c = n + i64::from(head) + i64::from(tail);
                    counts.push(c);
                    if !count {
                        for k in 0..c {
                            let mut a = s + (k - i64::from(head)) * g;
                            let mut b = a + g;
                            let first = head && k == 0;
                            let last = tail && k == c - 1;
                            if first {
                                a = s0;
                                b = head_end;
                            } else if last {
                                b = e0;
                            }
                            out.push([a, b, q]);
                        }
                    }
                    continue;
                }
                let short = n == 0;
                let n = if short { 1 } else { n };
                counts.push(n);
                if count {
                    continue;
                }
                if short {
                    let (mut a, mut b) = (s0, e0);
                    if ends == "min" {
                        let least = (-((-g).div_euclid(4))).max(1);
                        if b - a < least {
                            a += (b - a - least).div_euclid(2);
                            b = a + least;
                        }
                    }
                    out.push([a, b, q]);
                } else {
                    for k in 0..n {
                        let a = s + k * g;
                        out.push([a, a + g, q]);
                    }
                }
            }
        }
    }
    (out, counts)
}

/// One stretch chopped by placed repeats: the squares whose middle is in it.
#[allow(clippy::too_many_arguments)]
fn chop_grid(
    sq: &[[i64; 2]],
    mids: &[i64],
    s0: i64,
    e0: i64,
    q: i64,
    count: bool,
    out: &mut Vec<N3>,
    counts: &mut Vec<i64>,
) {
    let lo = mids.partition_point(|m| *m < 2 * s0);
    let hi = mids.partition_point(|m| *m < 2 * e0);
    let n = hi.saturating_sub(lo);
    counts.push(n as i64);
    if !count {
        for s in &sq[lo..lo + n] {
            out.push([s[0], s[1], q]);
        }
    }
}

// ================================================================ pasted notes

/// Packed notes with the track column start with this.
pub const TRACKS: &str = "t:";

/// `pack_notes`' text → `(start, end, row, velocity, track)` rows.
pub fn unpack_notes(text: &str) -> Option<Vec<[i64; 5]>> {
    let tracks = text.starts_with(TRACKS);
    let body = if tracks { &text[TRACKS.len()..] } else { text };
    let raw = crate::codec::b64decode_py(body)?;
    let data = crate::codec::zlib_decompress(&raw, 1 << 30)?;
    if data.len() % 4 != 0 {
        return None;
    }
    let cols = if tracks { 5 } else { 4 };
    let vals: Vec<i64> = data
        .chunks_exact(4)
        .map(|c| i64::from(i32::from_le_bytes([c[0], c[1], c[2], c[3]])))
        .collect();
    if !vals.len().is_multiple_of(cols) {
        return None;
    }
    Some(
        vals.chunks_exact(cols)
            .map(|r| [r[0], r[1], r[2], r[3], if tracks { r[4] } else { 0 }])
            .collect(),
    )
}

/// `(start, end, row, velocity, track)` rows → text for the save file.
pub fn pack_notes(rows: &[[i64; 5]]) -> String {
    let mut data = Vec::with_capacity(rows.len() * 20);
    for r in rows {
        for v in r {
            data.extend_from_slice(&(*v as i32).to_le_bytes());
        }
    }
    let z = crate::codec::zlib_compress(&data);
    format!("{TRACKS}{}", crate::codec::b64encode(&z))
}

/// Packed notes Spiderweb can use.
pub fn check_notes(text: &str) -> bool {
    let Some(rows) = unpack_notes(text) else {
        return false;
    };
    !rows.is_empty()
        && rows.iter().all(|r| {
            r[0] >= 0 && r[1] > r[0] && r[2] >= 0 && (1..=127).contains(&r[3]) && r[4] >= 0
        })
}

/// A pasted-notes shape's `(start, end, pitch, velocity, track)` notes
/// where its box is now.
pub fn block_notes(sh: &Value, ppq: u32) -> Vec<[i64; 5]> {
    let Some(rows) = gets(sh, "notes").and_then(unpack_notes) else {
        return Vec::new();
    };
    if rows.is_empty() {
        return Vec::new();
    }
    let p = super::shape_pts(sh);
    let [[b0, p0], [b1, p1], [b2, p2]] = [p[0], p[1], p[2]];
    let (ub, up, vb, vp) = (b1 - b0, p1 - p0, b2 - b0, p2 - p0);
    let t_all = rows.iter().map(|r| r[1]).max().expect("rows") as f64;
    let k_all = (rows.iter().map(|r| r[2]).max().expect("rows") + 1) as f64;
    let ppqf = f64::from(ppq);
    let mut r: Vec<P> = Vec::with_capacity(rows.len() * 2);
    for row in &rows {
        let v = (row[2] as f64 + 0.5) / k_all;
        let end = |t: i64| {
            let u = t as f64 / t_all;
            [(b0 + u * ub + v * vb) * ppqf, p0 + u * up + v * vp]
        };
        let (mut a, mut b) = (end(row[0]), end(row[1]));
        if a[0] > b[0] || (a[0] == b[0] && a[1] > b[1]) {
            std::mem::swap(&mut a, &mut b);
        }
        r.push(a);
        r.push(b);
    }
    let first: Vec<usize> = (0..rows.len()).map(|i| i * 2).collect();
    let (raw, per) = parts_notes(&r, &first, &vec![false; rows.len()]);
    let mut out = Vec::with_capacity(raw.len());
    let mut at = 0;
    for (row, &n) in rows.iter().zip(&per) {
        for x in &raw[at..at + n] {
            out.push([x[0], x[1], x[2], row[3], row[4]]);
        }
        at += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_notes_read_back_and_python_packs_read() {
        let rows = vec![
            [0, 240, 0, 100, 0],
            [240, 480, 2, 90, 1],
            [480, 960, 5, 80, 3],
        ];
        let t = pack_notes(&rows);
        assert!(t.starts_with("t:"));
        assert_eq!(unpack_notes(&t).unwrap(), rows);
        assert!(check_notes(&t));
        if std::env::var("PRINT_PACK").is_ok() {
            println!("PACK {t}");
        }
        // pack_notes([[0, 240, 0, 100, 0], [240, 480, 2, 90, 1]]) in Spiderweb
        let py = "t:eAFjYGBg+ADEIJACocD8B4wMDExAfhQQA5kMAFCTA4M=";
        assert_eq!(unpack_notes(py).unwrap(), rows[..2].to_vec());
        assert!(!check_notes("t:abc"));
        assert_eq!(py_set_order(&[3, 11, 0, 5]), vec![11, 0, 3, 5]);
    }
}
