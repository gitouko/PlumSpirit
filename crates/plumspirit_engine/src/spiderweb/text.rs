//! Text shapes (text.py): typed text turned into a custom shape of the
//! letters' outlines. The notes only need the shape's stored strokes and
//! settings (`text_polys`, `threshold_spans`); laying out new text needs a
//! font (`FontSource`, given by the app: installed fonts read with skrifa
//! instead of Spiderweb's Windows GDI).

use super::np::{dist, hypot, py_sum, pyround};
use super::paths::P;
use super::{fnum, inum, pts_of, pts_value};
use crate::json::Value;

pub const UNITS: [&str; 2] = ["font", "rows"];
pub const TEXT_ALIGNS: [&str; 3] = ["left", "center", "right"];
/// PlumSpirit's capitalisation choices.
pub const CAPS: [&str; 4] = ["none", "upper", "lower", "title"];
/// Each key is looked at on this many lines for the threshold.
pub const SUB_ROWS: usize = 20;

#[inline]
fn pmax(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}
#[inline]
fn pmin(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// Text settings from a file (None: broken).
pub fn clean_text(tx: &Value) -> Option<Value> {
    if !matches!(tx, Value::Obj(_)) {
        return None;
    }
    let mut out = Value::obj();
    out.set(
        "text",
        Value::Str(
            tx.get("text")
                .map_or_else(String::new, super::pattern::py_str),
        ),
    );
    let font = match tx.get("font").filter(|f| f.truthy()) {
        Some(f) => super::pattern::py_str(f),
        None => "Arial".to_owned(),
    };
    out.set("font", Value::Str(font));
    let mut nums = Vec::new();
    for (key, d) in [
        ("size", 24.0),
        ("tracking", 0.0),
        ("leading", 100.0),
        ("threshold", 50.0),
        ("grow", 0.0),
    ] {
        let v = match tx.get(key) {
            None => d,
            Some(v) => fnum(v)?,
        };
        nums.push((key, v));
    }
    let unit = super::gets(tx, "unit")
        .filter(|u| UNITS.contains(u))
        .unwrap_or("font");
    let align = super::gets(tx, "align")
        .filter(|u| TEXT_ALIGNS.contains(u))
        .unwrap_or("left");
    let weight = match tx.get("weight") {
        None => 400,
        Some(v) => inum(v)?,
    }
    .clamp(1, 1000);
    let italic = tx.get("italic").is_some_and(Value::truthy);
    for (key, v) in nums {
        let v = if key == "threshold" {
            pmin(100.0, pmax(0.0, v))
        } else {
            v
        };
        out.set(key, Value::Num(v));
    }
    out.set("unit", Value::Str(unit.to_owned()));
    out.set("align", Value::Str(align.to_owned()));
    out.set("weight", Value::Num(weight as f64));
    out.set("italic", Value::Bool(italic));
    let bbox: Vec<f64> = tx
        .get("bbox")?
        .as_arr()?
        .iter()
        .map(fnum)
        .collect::<Option<Vec<f64>>>()?;
    let or = |v: f64, d: f64| if v == 0.0 { d } else { v };
    let cap = or(
        match tx.get("cap") {
            None => 0.7,
            Some(v) => fnum(v)?,
        },
        0.7,
    );
    let k = or(
        match tx.get("k") {
            None => 1.0,
            Some(v) => fnum(v)?,
        },
        1.0,
    );
    let mut holes: Vec<i64> = match tx.get("holes") {
        None => Vec::new(),
        Some(v) => v.as_arr()?.iter().map(inum).collect::<Option<Vec<i64>>>()?,
    };
    holes.sort_unstable();
    holes.dedup();
    if bbox.len() != 4 {
        return None;
    }
    out.set(
        "bbox",
        Value::Arr(bbox.into_iter().map(Value::Num).collect()),
    );
    out.set("cap", Value::Num(cap));
    out.set("k", Value::Num(k));
    out.set(
        "holes",
        Value::Arr(holes.into_iter().map(|h| Value::Num(h as f64)).collect()),
    );
    // PlumSpirit's text styles (kept for retyping; the notes only use the
    // strokes): underline, strikeout, capitalisation.
    for key in ["underline", "strike"] {
        if tx.get(key) == Some(&Value::Bool(true)) {
            out.set(key, Value::Bool(true));
        }
    }
    if let Some(c) = super::gets(tx, "caps").filter(|c| CAPS.contains(c) && *c != "none") {
        out.set("caps", Value::Str(c.to_owned()));
    }
    Some(out)
}

/// The text with its capitalisation applied (PlumSpirit).
pub fn shown_text(tx: &Value) -> String {
    let text = super::gets(tx, "text").unwrap_or("");
    match super::gets(tx, "caps").unwrap_or("none") {
        "upper" => text.to_uppercase(),
        "lower" => text.to_lowercase(),
        "title" => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for ch in text.chars() {
                if start && ch.is_alphabetic() {
                    out.extend(ch.to_uppercase());
                } else if ch.is_alphabetic() {
                    out.extend(ch.to_lowercase());
                } else {
                    out.push(ch);
                }
                start = !ch.is_alphanumeric() && ch != '\'';
            }
            out
        }
        _ => text.to_owned(),
    }
}

fn line_dist(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let ll = hypot(dx, dy);
    if ll < 1e-12 {
        return dist(p, a);
    }
    ((p[0] - a[0]) * dy - (p[1] - a[1]) * dx).abs() / ll
}

/// A Bezier curve as points: straight pieces one step, bends enough steps.
pub fn flatten(pts: &[P], tol: f64) -> Vec<P> {
    let mut out = vec![pts[0]];
    let mut i = 0;
    while i + 3 < pts.len() {
        let (p0, p1, p2, p3) = (pts[i], pts[i + 1], pts[i + 2], pts[i + 3]);
        let bend = pmax(line_dist(p1, p0, p3), line_dist(p2, p0, p3));
        let n: i64 = if bend < 1e-9 {
            1
        } else {
            (((bend / tol).sqrt() * 2.0).ceil() as i64).clamp(2, 24)
        };
        for k in 1..=n {
            let t = k as f64 / n as f64;
            let a = (1.0 - t).powf(3.0);
            let b = 3.0 * (1.0 - t).powf(2.0) * t;
            let c = 3.0 * (1.0 - t) * t * t;
            let d = t.powf(3.0);
            out.push([
                a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0],
                a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1],
            ]);
        }
        i += 3;
    }
    out
}

/// Signed area (Python's compensated `sum`).
pub fn area(poly: &[P]) -> f64 {
    py_sum(
        poly.windows(2)
            .map(|w| w[0][0] * w[1][1] - w[1][0] * w[0][1]),
    ) / 2.0
}

pub fn winding(poly: &[P], x: f64, y: f64) -> i64 {
    let mut w = 0;
    for s in poly.windows(2) {
        let ([xa, ya], [xb, yb]) = (s[0], s[1]);
        if (ya <= y) != (yb <= y) && x < xa + (xb - xa) * (y - ya) / (yb - ya) {
            w += if yb > ya { 1 } else { -1 };
        }
    }
    w
}

/// Which contours are holes (inside an odd number of the same letter's
/// other contours).
pub fn find_holes(contours: &[(usize, Vec<P>)]) -> Vec<usize> {
    let flat: Vec<Vec<P>> = contours.iter().map(|(_, c)| flatten(c, 0.004)).collect();
    let mut holes = Vec::new();
    for (i, (g, _)) in contours.iter().enumerate() {
        let [x, y] = flat[i][0];
        let depth = contours
            .iter()
            .enumerate()
            .filter(|(j, (h, _))| *j != i && h == g && winding(&flat[*j], x, y) != 0)
            .count();
        if depth % 2 == 1 {
            holes.push(i);
        }
    }
    holes
}

/// A closed polygon grown by d (negative: shrunk), sharp corners bevelled.
pub fn offset(poly: &[P], d: f64) -> Vec<P> {
    let pts: Vec<P> = (0..poly.len().saturating_sub(1))
        .filter(|&i| dist(poly[i], poly[i + 1]) > 1e-9)
        .map(|i| poly[i])
        .collect();
    let n = pts.len();
    if n < 3 {
        return poly.to_vec();
    }
    let mut closed = pts.clone();
    closed.push(pts[0]);
    let s = if area(&closed) > 0.0 { 1.0 } else { -1.0 };
    let normals: Vec<P> = (0..n)
        .map(|i| {
            let ([xa, ya], [xb, yb]) = (pts[i], pts[(i + 1) % n]);
            let ll = hypot(xb - xa, yb - ya);
            [s * (yb - ya) / ll, -s * (xb - xa) / ll]
        })
        .collect();
    let mut out = Vec::new();
    for i in 0..n {
        let [ax, ay] = normals[(i + n - 1) % n];
        let [bx, by] = normals[i];
        let [x, y] = pts[i];
        let dot = ax * bx + ay * by;
        if dot > -0.5 {
            let m = d / (1.0 + dot);
            out.push([x + (ax + bx) * m, y + (ay + by) * m]);
        } else {
            out.push([x + ax * d, y + ay * d]);
            out.push([x + bx * d, y + by * d]);
        }
    }
    out.push(out[0]);
    out
}

/// The letters' outlines as closed polygons in beats / pitch, grown by the
/// grow setting.
pub fn text_polys(sh: &Value) -> Vec<Vec<P>> {
    let tx = sh.get("text").expect("text");
    let p = super::shape_pts(sh);
    let ([b0, p0], [b1, p1], [b2, p2]) = (p[0], p[1], p[2]);
    let (ub, up, vb, vp) = (b1 - b0, p1 - p0, b2 - b0, p2 - p0);
    let mut polys: Vec<Vec<P>> = Vec::new();
    for st in sh.get("strokes").and_then(Value::as_arr).unwrap_or(&[]) {
        let pts = st.get("pts").and_then(pts_of).unwrap_or_default();
        if pts.is_empty() {
            polys.push(Vec::new());
            continue;
        }
        polys.push(
            flatten(&pts, 0.002)
                .iter()
                .map(|&[u, v]| [b0 + u * ub + v * vb, p0 + u * up + v * vp])
                .collect(),
        );
    }
    let grow = tx.get("grow").and_then(fnum).unwrap_or(0.0);
    if grow != 0.0 {
        let k = tx.get("k").and_then(fnum).unwrap_or(1.0);
        let holes: Vec<i64> = tx
            .get("holes")
            .and_then(Value::as_arr)
            .map(|a| a.iter().filter_map(inum).collect())
            .unwrap_or_default();
        polys = polys
            .iter()
            .enumerate()
            .map(|(i, poly)| {
                let scaled: Vec<P> = poly.iter().map(|&[b, p]| [b / k, p]).collect();
                let d = if holes.contains(&(i as i64)) {
                    -grow
                } else {
                    grow
                };
                offset(&scaled, d)
                    .iter()
                    .map(|&[x, y]| [x * k, y])
                    .collect()
            })
            .collect();
    }
    polys
}

/// Where the line at height y is inside (nonzero rule).
fn line_spans(edges: &[(P, P)], y: f64) -> Vec<[f64; 2]> {
    let mut cross: Vec<(f64, i64)> = Vec::new();
    for &([xa, ya], [xb, yb]) in edges {
        if (ya <= y) != (yb <= y) {
            cross.push((
                xa + (xb - xa) * (y - ya) / (yb - ya),
                if yb > ya { 1 } else { -1 },
            ));
        }
    }
    cross.sort_by(|a, b| super::shrink::fcmp(a.0, b.0).then(a.1.cmp(&b.1)));
    let mut out = Vec::new();
    let (mut w, mut start) = (0i64, 0.0f64);
    for (x, d) in cross {
        let before = w;
        w += d;
        if before == 0 && w != 0 {
            start = x;
        } else if before != 0 && w == 0 {
            out.push([start, x]);
        }
    }
    out
}

/// Time ranges (beats) where at least `threshold` % of key q's height is
/// inside the letters.
pub fn threshold_spans(polys: &[Vec<P>], q: i64, threshold: f64) -> Vec<[f64; 2]> {
    let lo = q as f64 - 0.5;
    let hi = lo + 1.0;
    let edges: Vec<(P, P)> = polys
        .iter()
        .flat_map(|p| p.windows(2).map(|w| (w[0], w[1])))
        .filter(|(a, b)| a[1] != b[1] && pmin(a[1], b[1]) < hi && pmax(a[1], b[1]) > lo)
        .collect();
    if edges.is_empty() {
        return Vec::new();
    }
    let need = ((threshold / 100.0 * SUB_ROWS as f64 - 1e-9).ceil() as i64).max(1);
    let mut events: Vec<(f64, i64)> = Vec::new();
    for j in 0..SUB_ROWS {
        for [a, b] in line_spans(&edges, lo + (j as f64 + 0.5) / SUB_ROWS as f64) {
            events.push((a, 1));
            events.push((b, -1));
        }
    }
    events.sort_by(|a, b| super::shrink::fcmp(a.0, b.0).then(a.1.cmp(&b.1)));
    let mut out: Vec<[f64; 2]> = Vec::new();
    let (mut count, mut start) = (0i64, 0.0f64);
    for (x, d) in events {
        let before = count;
        count += d;
        if before < need && need <= count {
            start = x;
        } else if count < need && need <= before {
            if let Some(l) = out.last_mut()
                && start - l[1] < 1e-12
            {
                l[1] = x;
            } else if x > start {
                out.push([start, x]);
            }
        }
    }
    out
}

// ---------------------------------------------------------------- layout (new text)

/// A font for laying out text, in em units (1.0 = the font size, y up).
pub trait FontSource {
    /// (advance, contours as closed cubic Bezier point lists).
    fn glyph(&self, ch: char) -> (f64, Vec<Vec<P>>);
    fn kerning(&self, a: char, b: char) -> f64;
    /// Baseline to baseline.
    fn line_height(&self) -> f64;
    /// Capital letter height.
    fn cap(&self) -> f64;
    /// Underline (offset of its top from the baseline, thickness), em.
    fn underline(&self) -> (f64, f64) {
        (-0.1, 0.05)
    }
    /// Strikeout (offset of its top from the baseline, thickness), em.
    fn strikeout(&self) -> (f64, f64) {
        (0.3, 0.05)
    }
}

/// A rectangle as a closed contour of straight Bezier pieces.
fn rect_contour(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P> {
    let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]];
    let mut out = vec![corners[0]];
    for w in corners.windows(2) {
        let (a, b) = (w[0], w[1]);
        out.push([a[0] + (b[0] - a[0]) / 3.0, a[1] + (b[1] - a[1]) / 3.0]);
        out.push([
            a[0] + 2.0 * (b[0] - a[0]) / 3.0,
            a[1] + 2.0 * (b[1] - a[1]) / 3.0,
        ]);
        out.push(b);
    }
    out
}

/// The text laid out in em units: contours `(glyph number, Bezier points)`.
pub fn layout(tx: &Value, font: &dyn FontSource) -> Vec<(usize, Vec<P>)> {
    let leading = tx.get("leading").and_then(fnum).unwrap_or(100.0);
    let step = font.line_height() * leading / 100.0;
    let track = tx.get("tracking").and_then(fnum).unwrap_or(0.0) / 1000.0;
    let align = super::gets(tx, "align").unwrap_or("left");
    let text = shown_text(tx);
    let decorations: Vec<(f64, f64)> = [
        ("underline", font.underline()),
        ("strike", font.strikeout()),
    ]
    .into_iter()
    .filter(|(k, _)| tx.get(k) == Some(&Value::Bool(true)))
    .map(|(_, d)| d)
    .collect();
    let mut contours = Vec::new();
    let mut g = 0;
    for (n, line) in text.split('\n').enumerate() {
        let y = -(n as f64) * step;
        let mut xs = Vec::new();
        let mut x = 0.0;
        let mut prev: Option<char> = None;
        for ch in line.chars() {
            if let Some(p) = prev {
                x += font.kerning(p, ch) + track;
            }
            xs.push(x);
            x += font.glyph(ch).0;
            prev = Some(ch);
        }
        let shift = match align {
            "center" => -x / 2.0,
            "right" => -x,
            _ => 0.0,
        };
        for (ch, cx) in line.chars().zip(xs) {
            for c in font.glyph(ch).1 {
                contours.push((
                    g,
                    c.iter()
                        .map(|&[px, py]| [px + cx + shift, py + y])
                        .collect(),
                ));
            }
            g += 1;
        }
        // PlumSpirit: underline / strikeout across the line's letters
        if x > 0.0 {
            for &(top, thick) in &decorations {
                let thick = pmax(thick, 0.01);
                contours.push((g, rect_contour(shift, y + top - thick, shift + x, y + top)));
                g += 1;
            }
        }
    }
    contours
}

/// Lay out the text into custom shape `sh` at `axes` (origin, x axis, y
/// axis: em units → beats / pitch). False when there's nothing to see.
pub fn build(sh: &mut Value, tx: &Value, axes: (P, P, P), font: &dyn FontSource) -> bool {
    let contours = layout(tx, font);
    if contours.is_empty() {
        return false;
    }
    let pts: Vec<P> = contours
        .iter()
        .flat_map(|(_, c)| flatten(c, 0.004))
        .collect();
    let fold = |f: fn(f64, f64) -> f64, i: usize, init: f64| pts.iter().map(|p| p[i]).fold(init, f);
    let (mut x0, mut x1) = (
        fold(pmin, 0, f64::INFINITY),
        fold(pmax, 0, f64::NEG_INFINITY),
    );
    let (mut y0, mut y1) = (
        fold(pmin, 1, f64::INFINITY),
        fold(pmax, 1, f64::NEG_INFINITY),
    );
    if x1 - x0 < 1e-6 {
        (x0, x1) = (x0 - 0.01, x1 + 0.01);
    }
    if y1 - y0 < 1e-6 {
        (y0, y1) = (y0 - 0.01, y1 + 0.01);
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let strokes = contours
        .iter()
        .map(|(_, c)| {
            let mut st = Value::obj();
            st.set("kind", Value::Str("curve".into()));
            st.set(
                "pts",
                pts_value(
                    &c.iter()
                        .map(|&[x, y]| [pyround((x - x0) / w, 7), pyround((y - y0) / h, 7)])
                        .collect::<Vec<P>>(),
                ),
            );
            st
        })
        .collect();
    let (o, xa, ya) = axes;
    let to_roll = |x: f64, y: f64| [o[0] + x * xa[0] + y * ya[0], o[1] + x * xa[1] + y * ya[1]];
    sh.set("strokes", Value::Arr(strokes));
    sh.set(
        "pts",
        pts_value(&[to_roll(x0, y0), to_roll(x1, y0), to_roll(x0, y1)]),
    );
    let mut t = tx.clone();
    t.set(
        "bbox",
        Value::Arr([x0, y0, x1, y1].into_iter().map(Value::Num).collect()),
    );
    t.set("cap", Value::Num(font.cap()));
    t.set(
        "holes",
        Value::Arr(
            find_holes(&contours)
                .into_iter()
                .map(|h| Value::Num(h as f64))
                .collect(),
        ),
    );
    sh.set("text", t);
    true
}
