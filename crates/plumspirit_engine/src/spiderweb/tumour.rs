//! Tumours: bumps along a line (port of Spiderweb `tumour.py`, MIT © Kanade
//! Tachibana; the idea credited there to Mapping Tools by OliBomby).

use super::curves::arc_points_step;
use super::np::{dist, hypot, interp, linspace, np_hypot};
use super::paths::P;
use super::rng::PyRandom;
use super::{fnum, inum};
use crate::json::Value;

const SHAPES: [&str; 4] = ["triangle", "square", "circle", "parabola"];
const SIDES: [&str; 4] = ["alt", "left", "right", "random"];
const WRAPS: [&str; 2] = ["simple", "wrap"];
const MAX_TUMOURS: usize = 20_000;
const GRAPH_KEYS: [&str; 5] = ["size", "length", "dist", "rot", "slant"];
const GRAPH_LIMIT: f64 = 10.0;

/// Tumour settings from a file (None when there are none).
pub fn clean_tumour(tm: Option<&Value>) -> Option<Value> {
    let tm = tm?;
    if !matches!(tm, Value::Obj(_)) {
        return None;
    }
    let pick = |key: &str, choices: &[&str], d: &str| -> Value {
        match tm.get(key).and_then(Value::as_str) {
            Some(v) if choices.contains(&v) => Value::Str(v.to_owned()),
            _ => Value::Str(d.to_owned()),
        }
    };
    let num = |key: &str, d: f64, lo: f64, hi: f64| -> Value {
        match tm.get(key).map_or(Some(d), fnum) {
            Some(v) if v.is_finite() => Value::Num(v.max(lo).min(hi)),
            _ => Value::Num(d),
        }
    };
    let mut out = Value::obj();
    out.set("on", Value::Bool(tm.get("on") != Some(&Value::Bool(false))));
    out.set("shape", pick("shape", &SHAPES, "triangle"));
    out.set("size", num("size", 3.0, -1000.0, 1000.0));
    out.set("length", num("length", 0.125, 0.0, 1e6));
    out.set("dist", num("dist", 0.125, 1e-9, 1e6));
    out.set("side", pick("side", &SIDES, "alt"));
    out.set("wrap", pick("wrap", &WRAPS, "simple"));
    out.set("start", num("start", 0.0, 0.0, 1.0));
    out.set("end", num("end", 1.0, 0.0, 1.0));
    out.set("ease", num("ease", 0.0, 0.0, 1e6));
    out.set("rot", num("rot", 0.0, -180.0, 180.0));
    out.set("slant", num("slant", 0.0, -1.0, 1.0));
    out.set(
        "fit",
        Value::Bool(tm.get("fit") == Some(&Value::Bool(true))),
    );
    let seed = tm.get("seed").map_or(Some(1), inum).unwrap_or(1);
    out.set("seed", Value::Num(seed as f64));
    out.set(
        "mirror",
        Value::Bool(tm.get("mirror") == Some(&Value::Bool(true))),
    );
    out.set("k", num("k", 0.25, 1e-9, 1e9));
    if let Some(Value::Obj(gs)) = tm.get("graphs") {
        let mut graphs = Value::obj();
        let mut any = false;
        for key in GRAPH_KEYS {
            if let Some(g) = gs
                .iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, g)| clean_graph(g))
            {
                graphs.set(key, g);
                any = true;
            }
        }
        if any {
            out.set("graphs", graphs);
        }
    }
    Some(out)
}

/// A graph `[[u, f], …]` from u = 0 to 1 (None when unusable or flat at 100 %).
pub fn clean_graph(g: &Value) -> Option<Value> {
    let a = g.as_arr()?;
    let mut pts: Vec<[f64; 2]> = Vec::with_capacity(a.len());
    for p in a {
        let q = p.as_arr()?;
        if q.len() != 2 {
            return None;
        }
        let (u, f) = (fnum(&q[0])?, fnum(&q[1])?);
        // max-then-min (not clamp): a NaN becomes the bound, as in Python.
        #[allow(clippy::manual_clamp)]
        pts.push([u.max(0.0).min(1.0), f.max(-GRAPH_LIMIT).min(GRAPH_LIMIT)]);
    }
    if pts.len() < 2 || !pts.iter().all(|p| p[0].is_finite() && p[1].is_finite()) {
        return None;
    }
    pts[0][0] = 0.0;
    let n = pts.len();
    pts[n - 1][0] = 1.0;
    for i in 1..n {
        pts[i][0] = pts[i][0].max(pts[i - 1][0]);
    }
    if pts.iter().all(|p| (p[1] - 1.0).abs() < 1e-12) {
        return None;
    }
    Some(super::pts_value(&pts))
}

fn graph_fn(tm: &Value, key: &str, total: f64) -> Option<(Vec<f64>, Vec<f64>)> {
    let g = tm.get("graphs")?.get(key)?;
    let pts = super::pts_of(g)?;
    if pts.is_empty() {
        return None;
    }
    Some((
        pts.iter().map(|p| p[0] * total).collect(),
        pts.iter().map(|p| p[1]).collect(),
    ))
}

#[inline]
fn gv(g: &(Vec<f64>, Vec<f64>), d: f64) -> f64 {
    interp(d, &g.0, &g.1)
}

fn graph_starts(
    dg: &(Vec<f64>, Vec<f64>),
    lo: f64,
    hi: f64,
    dist: f64,
    fit: bool,
    even: bool,
) -> (Vec<f64>, f64) {
    if hi - lo < 1e-9 {
        return (vec![lo], 1.0);
    }
    let d = linspace(lo, hi, 4097);
    let floor = ((hi - lo) / MAX_TUMOURS as f64).max(1e-9);
    let rate: Vec<f64> = d
        .iter()
        .map(|&x| 1.0 / (dist * gv(dg, x)).max(floor))
        .collect();
    let mut count = vec![0.0];
    let mut acc = 0.0;
    for i in 1..d.len() {
        acc += (rate[i] + rate[i - 1]) / 2.0 * (d[i] - d[i - 1]);
        count.push(acc);
    }
    let total = count[count.len() - 1];
    let mut stretch = 1.0;
    let marks: Vec<f64> = if fit {
        let n = if even {
            (2.0 * (total / 2.0).round_ties_even()).max(2.0)
        } else {
            total.round_ties_even().max(1.0)
        };
        stretch = total / n;
        linspace(0.0, total, n as usize + 1)
    } else {
        let m = ((total + 1e-3).floor() as i64).min(MAX_TUMOURS as i64 - 1);
        (0..=m.max(-1)).map(|i| i as f64).collect()
    };
    (
        marks.iter().map(|&m| interp(m, &count, &d)).collect(),
        stretch,
    )
}

fn template(shape: &str, length: f64, size: f64, slant: f64) -> Vec<P> {
    match shape {
        "triangle" => vec![[0.0, 0.0], [length / 2.0, size], [length, 0.0]],
        "square" => {
            let m = length / 2.0 * slant;
            vec![[0.0, 0.0], [m, size], [length - m, size], [length, 0.0]]
        }
        "parabola" => (0..17)
            .map(|t| {
                let tf = t as f64;
                [
                    length * tf / 16.0,
                    size * 4.0 * (tf / 16.0) * (1.0 - tf / 16.0),
                ]
            })
            .collect(),
        _ => {
            if size.abs() < 1e-12 {
                return vec![[0.0, 0.0], [length, 0.0]];
            }
            arc_points_step(
                &[[0.0, 0.0], [length / 2.0, size], [length, 0.0]],
                1.0,
                10.0f64.to_radians(),
            )
        }
    }
}

fn cut(pts: Vec<P>, x_end: f64) -> Vec<P> {
    if x_end >= pts[pts.len() - 1][0] - 1e-12 && pts.iter().all(|p| p[0] <= x_end + 1e-12) {
        return pts;
    }
    let mut out = vec![pts[0]];
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b[0] <= x_end + 1e-12 {
            out.push(b);
            continue;
        }
        if a[0] < x_end && x_end < b[0] {
            let u = (x_end - a[0]) / (b[0] - a[0]);
            out.push([x_end, a[1] + (b[1] - a[1]) * u]);
        }
        break;
    }
    out
}

/// A path measured along its length.
pub struct Walk {
    pub pts: Vec<P>,
    pub cum: Vec<f64>,
    pub total: f64,
    pub closed: bool,
    dirs: Vec<P>,
}

impl Walk {
    pub fn new(pts: Vec<P>) -> Walk {
        let mut cum = vec![0.0];
        for w in pts.windows(2) {
            cum.push(cum[cum.len() - 1] + dist(w[0], w[1]));
        }
        let total = cum[cum.len() - 1];
        let closed = pts.len() > 2 && dist(pts[0], pts[pts.len() - 1]) < 1e-9;
        let dirs = (0..pts.len() - 1)
            .map(|i| {
                let (a, b) = (pts[i], pts[i + 1]);
                let n = dist(a, b);
                if n != 0.0 {
                    [(b[0] - a[0]) / n, (b[1] - a[1]) / n]
                } else {
                    [1.0, 0.0]
                }
            })
            .collect();
        Walk {
            pts,
            cum,
            total,
            closed,
            dirs,
        }
    }

    fn seg(&self, d: f64) -> usize {
        let i = self.cum.partition_point(|&c| c <= d) as i64 - 1;
        i.clamp(0, self.pts.len() as i64 - 2) as usize
    }

    pub fn at(&self, d: f64) -> P {
        let i = self.seg(d);
        let (a, b) = (self.pts[i], self.pts[i + 1]);
        let n = self.cum[i + 1] - self.cum[i];
        let u = if n == 0.0 {
            0.0
        } else {
            ((d - self.cum[i]) / n).clamp(0.0, 1.0)
        };
        [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]
    }

    /// Unit direction at distance d (a corner: half way between its sides);
    /// the array version (`directions`) of the original.
    pub fn direction(&self, d: f64) -> P {
        let i = self.seg(d);
        let mut v = self.dirs[i];
        let mut before = i as i64 - 1;
        let mut corner = (d - self.cum[i]).abs() < 1e-12 && (i > 0 || self.closed);
        if self.closed {
            if i == 0 {
                before = self.pts.len() as i64 - 2;
            }
            if d >= self.total - 1e-12 {
                v = self.dirs[0];
                before = self.pts.len() as i64 - 2;
                corner = true;
            }
        }
        if corner && before >= 0 {
            let w = self.dirs[before as usize];
            let s = [v[0] + w[0], v[1] + w[1]];
            let n = np_hypot(s[0], s[1]); // np.hypot (array direction)
            if n > 1e-9 {
                return [s[0] / n, s[1] / n];
            }
        }
        v
    }

    /// The scalar `direction` of the original (used once, for a degenerate
    /// simple-wrap bump).
    fn direction_scalar(&self, d: f64) -> P {
        let i = self.seg(d);
        let v = self.dirs[i];
        let mut before: Option<usize> = None;
        if (d - self.cum[i]).abs() < 1e-12 {
            before = if i > 0 {
                Some(i - 1)
            } else if self.closed {
                Some(self.pts.len() - 2)
            } else {
                None
            };
        }
        let mut v = v;
        if self.closed && d >= self.total - 1e-12 {
            v = self.dirs[0];
            before = Some(self.pts.len() - 2);
        }
        if let Some(b) = before {
            let w = self.dirs[b];
            let s = [v[0] + w[0], v[1] + w[1]];
            let n = hypot(s[0], s[1]);
            if n > 1e-9 {
                return [s[0] / n, s[1] / n];
            }
        }
        v
    }

    /// Points strictly between distances d0 and d1.
    fn base(&self, d0: f64, d1: f64) -> Vec<P> {
        let a = self.cum.partition_point(|&c| c <= d0 + 1e-9);
        let b = self.cum.partition_point(|&c| c < d1 - 1e-9);
        if a < b {
            self.pts[a..b].to_vec()
        } else {
            Vec::new()
        }
    }
}

/// Where the bumps start: (range start, range end, closed loop all round,
/// the starts, how much Fit stretched the distance).
pub fn bump_starts(w: &Walk, tm: &Value) -> (f64, f64, bool, Vec<f64>, f64) {
    let g = |k: &str| tm.get(k).and_then(fnum).unwrap_or(0.0);
    let k = g("k");
    let (st, en) = (g("start"), g("end"));
    let lo = st.min(en) * w.total;
    let hi = st.max(en) * w.total;
    let dist = (g("dist") / k)
        .max((hi - lo) / MAX_TUMOURS as f64)
        .max(1e-9);
    let lp = w.closed && lo < 1e-9 && hi > w.total - 1e-9;
    let alt = tm.get("side").and_then(Value::as_str) == Some("alt");
    let fit = tm.get("fit") == Some(&Value::Bool(true));
    let mut stretch = 1.0;
    let starts = if let Some(dg) = graph_fn(tm, "dist", w.total) {
        let (s, f) = graph_starts(&dg, lo, hi, dist, fit, lp && alt);
        stretch = f;
        s
    } else if fit && hi - lo > 1e-9 {
        let mut n = ((hi - lo) / dist).round_ties_even().max(1.0);
        if lp && alt {
            n = (2.0 * ((hi - lo) / dist / 2.0).round_ties_even()).max(2.0);
        }
        stretch = (hi - lo) / n / dist;
        let nn = n as usize;
        (0..=nn).map(|i| lo + (hi - lo) * i as f64 / n).collect()
    } else {
        let mut v = Vec::new();
        let mut s = lo;
        while s <= hi + 1e-9 && v.len() < MAX_TUMOURS {
            v.push(s.min(hi));
            s += dist;
        }
        v
    };
    (lo, hi, lp, starts, stretch)
}

fn subdivide(bump: &[P], xs: &[f64]) -> Vec<P> {
    let mut out = vec![bump[0]];
    for w in bump.windows(2) {
        let ([xa, ya], [xb, yb]) = (w[0], w[1]);
        if (xb - xa).abs() > 1e-12 {
            let lo = xs.partition_point(|&x| x <= xa.min(xb) + 1e-12);
            let hi = xs.partition_point(|&x| x < xa.max(xb) - 1e-12);
            let mid: Vec<f64> = if lo < hi {
                xs[lo..hi].to_vec()
            } else {
                Vec::new()
            };
            let it: Box<dyn Iterator<Item = &f64>> = if xb > xa {
                Box::new(mid.iter())
            } else {
                Box::new(mid.iter().rev())
            };
            for &x in it {
                out.push([x, ya + (yb - ya) * (x - xa) / (xb - xa)]);
            }
        }
        out.push([xb, yb]);
    }
    out
}

fn sorted_set(v: impl IntoIterator<Item = f64>) -> Vec<f64> {
    let mut s: Vec<f64> = v.into_iter().collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s.dedup();
    s
}

/// Points in order: literal points, and blocks of bump points.
enum Piece {
    Lit(P),
    Bump(usize),
}

/// The path (beat, pitch points) with its tumours.
pub fn tumour_path(path: &[P], tm: &Value) -> Vec<P> {
    let g = |k: &str| tm.get(k).and_then(fnum).unwrap_or(0.0);
    let s_ = |k: &str| tm.get(k).and_then(Value::as_str).unwrap_or("");
    let k = g("k");
    let mut pts: Vec<P> = Vec::new();
    for p in path {
        let q = [p[0] / k, p[1]];
        if pts.last() != Some(&q) {
            pts.push(q);
        }
    }
    let size = g("size");
    if pts.len() < 2 || size.abs() < 1e-12 {
        return path.to_vec();
    }
    let w = Walk::new(pts);
    if w.total < 1e-12 {
        return path.to_vec();
    }
    let length = (g("length") / k).max(0.0);
    let (lo, hi, lp, starts, _) = bump_starts(&w, tm);
    let zg = graph_fn(tm, "size", w.total);
    let lg = graph_fn(tm, "length", w.total);
    let rg = graph_fn(tm, "rot", w.total);
    let sg = graph_fn(tm, "slant", w.total);
    let rot = g("rot").to_radians();
    let lean = rot.abs() > 1e-12;
    let (sin, cos) = (rot.sin(), rot.cos());
    let seed = tm.get("seed").and_then(inum).unwrap_or(1);
    let mut rnd = PyRandom::new(seed);
    let flip = if tm.get("mirror") == Some(&Value::Bool(true)) {
        -1.0
    } else {
        1.0
    };
    let side_mode = s_("side");
    let mut sides: Vec<f64> = Vec::with_capacity(starts.len());
    for i in 0..starts.len() {
        let side = match side_mode {
            "left" => 1.0,
            "right" => -1.0,
            "alt" => {
                if i % 2 == 0 {
                    1.0
                } else {
                    -1.0
                }
            }
            _ => rnd.choice2(1.0, -1.0),
        };
        sides.push(flip * side);
    }
    let fit_loop = lp && tm.get("fit") == Some(&Value::Bool(true));
    if fit_loop {
        let l = sides.len();
        sides[l - 1] = sides[0];
    }
    let ease = g("ease") / k;
    let grow = |d: f64| -> f64 {
        if ease < 1e-12 {
            return 1.0;
        }
        // max(0, min(1, (d - lo)/ease, (hi - d)/ease))
        let m = 1.0f64.min((d - lo) / ease).min((hi - d) / ease);
        m.max(0.0)
    };
    let eased = |bump: &[P], s: f64| -> Option<Vec<P>> {
        if ease < 1e-12 || !(s < lo + ease || s + bump[bump.len() - 1][0] > hi - ease) {
            return None;
        }
        let step = ease / 16.0;
        let mut out = vec![bump[0]];
        for wv in bump.windows(2) {
            let ([xa, ya], [xb, yb]) = (wv[0], wv[1]);
            let marks: Vec<f64> = if (xb - xa).abs() > 1e-12 {
                vec![
                    (lo + ease - s - xa) / (xb - xa),
                    (hi - ease - s - xa) / (xb - xa),
                ]
            } else {
                Vec::new()
            };
            let n = (((xb - xa).abs() / step) as i64).clamp(0, 64) as usize;
            let us = sorted_set(
                (1..=n)
                    .map(|j| j as f64 / (n + 1) as f64)
                    .chain(marks.into_iter().filter(|u| 1e-9 < *u && *u < 1.0 - 1e-9)),
            );
            for u in us {
                out.push([xa + (xb - xa) * u, ya + (yb - ya) * u]);
            }
            out.push([xb, yb]);
        }
        Some(
            out.iter()
                .map(|p| [p[0].max(lo - s).min(hi - s), p[1] * grow(s + p[0])])
                .collect(),
        )
    };
    let normal = |d: f64| -> P {
        let u = w.direction(d);
        [-u[1], u[0]]
    };
    let mut order: Vec<Piece> = Vec::new();
    let add = |order: &mut Vec<Piece>, v: Vec<P>| {
        for p in v {
            order.push(Piece::Lit(p));
        }
    };
    let mut bumps: Vec<P> = Vec::new();
    if starts[0] > 1e-9 {
        let mut v = vec![w.pts[0]];
        v.extend(w.base(0.0, starts[0]));
        add(&mut order, v);
    }
    if length < 1e-12 {
        // spikes
        if order.is_empty() && !fit_loop {
            add(&mut order, vec![w.pts[0]]);
        }
        for (i, &s) in starts.iter().enumerate() {
            let side = sides[i];
            let p = w.at(s);
            let nn = normal(s);
            let mut h = size * grow(s);
            if let Some(z) = &zg {
                h *= gv(z, s);
            }
            let q = if lean {
                let (ux, uy) = (nn[1], -nn[0]);
                let (sn, cs) = match &rg {
                    Some(r) => {
                        let rr = rot * gv(r, s);
                        (rr.sin(), rr.cos())
                    }
                    None => (sin, cos),
                };
                [
                    p[0] + (nn[0] * cs * side + ux * sn) * h,
                    p[1] + (nn[1] * cs * side + uy * sn) * h,
                ]
            } else {
                let h = h * side;
                [p[0] + nn[0] * h, p[1] + nn[1] * h]
            };
            order.push(Piece::Lit(q));
        }
        if !fit_loop {
            let mut v = w.base(starts[starts.len() - 1], w.total);
            v.push(w.pts[w.pts.len() - 1]);
            add(&mut order, v);
        }
    } else {
        let slant = g("slant");
        let shape_name = s_("shape");
        let fixed =
            (lg.is_none() && sg.is_none()).then(|| template(shape_name, length, size, slant));
        let simple = s_("wrap") == "simple";
        for i in 0..starts.len() {
            let (s, side) = (starts[i], sides[i]);
            let room = hi.min(starts.get(i + 1).copied().unwrap_or(f64::INFINITY));
            let li = match &lg {
                None => length,
                Some(l) => (length * gv(l, s)).max(0.0),
            };
            let e = (s + li).min(room);
            let nxt = starts.get(i + 1).copied().unwrap_or(w.total);
            if e - s < 1e-9 {
                add(&mut order, vec![w.at(s)]);
                add(&mut order, w.base(e, nxt));
                continue;
            }
            let tpl = match &fixed {
                Some(t) => t.clone(),
                None => {
                    let sl = match &sg {
                        None => slant,
                        #[allow(clippy::manual_clamp)] // NaN semantics as in Python
                        Some(sgf) => (slant * gv(sgf, s)).max(-1.0).min(1.0),
                    };
                    template(shape_name, li, size, sl)
                }
            };
            let mut bump = cut(tpl, room - s);
            let r = match &rg {
                None => rot,
                Some(rgf) => rot * gv(rgf, s + li / 2.0),
            };
            let (sn, cs) = if rg.is_none() {
                (sin, cos)
            } else {
                (r.sin(), r.cos())
            };
            if simple {
                if zg.is_some() {
                    let xs = sorted_set((0..17).map(|j| (e - s) * j as f64 / 16.0));
                    bump = subdivide(&bump, &xs);
                }
                let a = w.at(s);
                let b = w.at(e);
                let (mut ux, mut uy) = (b[0] - a[0], b[1] - a[1]);
                let n = hypot(ux, uy);
                if n > 1e-12 {
                    ux /= n;
                    uy /= n;
                } else {
                    let d = w.direction_scalar(s);
                    ux = d[0];
                    uy = d[1];
                }
                let stretch = if e - s > 1e-12 { n / (e - s) } else { 1.0 };
                let bump = eased(&bump, s).unwrap_or(bump);
                order.push(Piece::Bump(bump.len()));
                for p in &bump {
                    let mut x = p[0] * stretch;
                    let mut y = p[1];
                    if let Some(z) = &zg {
                        y *= gv(z, s + p[0]);
                    }
                    if lean {
                        let nx = x + y * sn;
                        y *= cs;
                        x = nx;
                    }
                    bumps.push([a[0] + ux * x - uy * y * side, a[1] + uy * x + ux * y * side]);
                }
            } else {
                let x0 = s + bump.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
                let x1 = s + bump.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
                let ca = w.cum.partition_point(|&c| c <= x0);
                let cb = w.cum.partition_point(|&c| c < x1);
                let mut xs: Vec<f64> = if ca < cb {
                    w.cum[ca..cb].iter().map(|c| c - s).collect()
                } else {
                    Vec::new()
                };
                if shape_name != "circle" {
                    xs.extend((0..17).map(|j| (e - s) * j as f64 / 16.0));
                }
                if let Some(eb) = eased(&bump, s) {
                    xs.extend(eb.iter().map(|p| p[0]));
                }
                let xs = sorted_set(xs);
                let bump = subdivide(&bump, &xs);
                order.push(Piece::Bump(bump.len()));
                for p in &bump {
                    let mut x = p[0];
                    let mut y = p[1] * grow(s + x);
                    if let Some(z) = &zg {
                        y *= gv(z, s + x);
                    }
                    if lean {
                        let nx = x + y * sn;
                        y *= cs;
                        x = nx;
                    }
                    let mut d = s + x;
                    if w.closed {
                        d = d.rem_euclid(w.total);
                    }
                    let dc = d.max(0.0).min(w.total);
                    let pp = w.at(dc);
                    let nn = normal(dc);
                    let over = d - dc;
                    let (px, py) = (pp[0] + nn[1] * over, pp[1] - nn[0] * over);
                    bumps.push([px + nn[0] * y * side, py + nn[1] * y * side]);
                }
            }
            if e >= hi - 1e-12 && nxt > e + 1e-12 {
                add(&mut order, vec![w.at(e)]);
            }
            add(&mut order, w.base(e, nxt));
        }
        add(&mut order, vec![w.pts[w.pts.len() - 1]]);
    }
    // Assemble, dropping exact repeats.
    let mut res: Vec<P> = Vec::with_capacity(order.len() + bumps.len());
    let mut bi = 0;
    for p in order {
        match p {
            Piece::Lit(q) => res.push(q),
            Piece::Bump(n) => {
                res.extend_from_slice(&bumps[bi..bi + n]);
                bi += n;
            }
        }
    }
    let mut out: Vec<P> = Vec::with_capacity(res.len());
    for q in res {
        if out.last() != Some(&q) {
            out.push(q);
        }
    }
    out.iter().map(|p| [p[0] * k, p[1]]).collect()
}
