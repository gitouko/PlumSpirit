//! Editing operations of the Spiderweb window: PlumSpirit's geometric
//! expansions (free rotation, Scale X / Y with negative values, Flip X / Y —
//! contract §034), box-placed shapes (circle, square, polygon / star /
//! crossing star, Hz bass, pasted notes) and live shapes (several shapes
//! turned into one fillable outline, Spiderweb's "Turn into live shape", and
//! split back). Everything works on cleaned shape objects and returns
//! cleaned ones (`clean_shape`), so a shape made here is the same as one
//! loaded from a file.
//!
//! Geometry convention (shared with v0.7's geometry engine,
//! `transform::geom::Geom::affine`): scale around a pivot first, then the
//! position; a negative scale mirrors. Rotation happens in the board's
//! on-screen proportions (`k` beats per key), so a circle stays a circle.

use super::custom::{box_stroke, pack_notes};
use super::paths::P;
use super::{
    clean_shape, fnum, getf, gets, hzbass, joined, kind, pattern, polygon, pts_of, pts_value,
    shape_strokes,
};
use crate::json::Value;

/// PlumSpirit's own settings on a shape (Spiderweb ignores unknown keys).
pub const PS: &str = "ps";
/// Most fixed colours a shape can ask for.
pub const MAX_FIXED_COLOUR: i64 = 1023;

/// The PlumSpirit extension object, cleaned (None: nothing to keep).
pub fn clean_ps(v: Option<&Value>) -> Option<Value> {
    let v = v?;
    if !matches!(v, Value::Obj(_)) {
        return None;
    }
    let mut out = Value::obj();
    if let Some(c) = v.get("colour").and_then(fnum)
        && c.is_finite()
        && c >= 0.0
    {
        out.set("colour", Value::Num(c.floor().min(MAX_FIXED_COLOUR as f64)));
    }
    match &out {
        Value::Obj(o) if !o.is_empty() => Some(out),
        _ => None,
    }
}

/// A shape's fixed colour (its notes all go to this colour, whatever the
/// channel mode), if it has one.
pub fn fixed_colour(sh: &Value) -> Option<i64> {
    sh.get(PS)
        .and_then(|p| p.get("colour"))
        .and_then(fnum)
        .map(|c| c as i64)
}

/// Sets (or clears) a shape's fixed colour.
pub fn set_fixed_colour(sh: &mut Value, colour: Option<i64>) {
    let mut ps = sh.get(PS).cloned().unwrap_or_else(Value::obj);
    match colour {
        Some(c) => ps.set("colour", Value::Num(c.clamp(0, MAX_FIXED_COLOUR) as f64)),
        None => {
            if let Value::Obj(o) = &mut ps {
                o.retain(|(k, _)| k != "colour");
            }
        }
    }
    match clean_ps(Some(&ps)) {
        Some(p) => sh.set(PS, p),
        None => {
            if let Value::Obj(o) = sh {
                o.retain(|(k, _)| k != PS);
            }
        }
    }
}

// ================================================================ transforms

/// A free transform: Scale X / Y (negative = mirrored; Flip = −1) around the
/// pivot, then a rotation (degrees, counter-clockwise as seen on screen).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xform {
    pub rotate: f64,
    pub sx: f64,
    pub sy: f64,
}

impl Default for Xform {
    fn default() -> Self {
        Xform {
            rotate: 0.0,
            sx: 1.0,
            sy: 1.0,
        }
    }
}

impl Xform {
    pub fn flip_x() -> Xform {
        Xform {
            sx: -1.0,
            ..Xform::default()
        }
    }
    pub fn flip_y() -> Xform {
        Xform {
            sy: -1.0,
            ..Xform::default()
        }
    }
    pub fn rotation(deg: f64) -> Xform {
        Xform {
            rotate: deg,
            ..Xform::default()
        }
    }
    /// Usable: finite, no zero scale (a zero would flatten the shape).
    pub fn valid(&self) -> bool {
        self.rotate.is_finite()
            && self.sx.is_finite()
            && self.sy.is_finite()
            && self.sx.abs() >= 1e-6
            && self.sy.abs() >= 1e-6
    }
    pub fn is_identity(&self) -> bool {
        self.rotate.rem_euclid(360.0).abs() < 1e-12 && self.sx == 1.0 && self.sy == 1.0
    }
    /// The point `q` (`[beat, pitch]`) moved; `k` = beats per key on screen.
    pub fn apply(&self, q: P, pivot: P, k: f64) -> P {
        let k = if k.is_finite() && k > 0.0 { k } else { 1.0 };
        let x = (q[0] - pivot[0]) / k * self.sx;
        let y = (q[1] - pivot[1]) * self.sy;
        let (s, c) = self.rotate.to_radians().sin_cos();
        let (rx, ry) = (x * c - y * s, x * s + y * c);
        [pivot[0] + rx * k, pivot[1] + ry]
    }
    /// How much a shape's own "beats per key" settings change (arcs,
    /// freehand, tumours, formulas, text): only a stretch changes them.
    fn k_factor(&self) -> f64 {
        (self.sx / self.sy).abs()
    }
}

/// The shapes' bounding box `(lo, hi)` (all strokes as drawn).
pub fn bounds(shapes: &[Value]) -> Option<(P, P)> {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for sh in shapes {
        for s in shape_strokes(sh) {
            for q in s {
                if q[0].is_finite() && q[1].is_finite() {
                    lo = [lo[0].min(q[0]), lo[1].min(q[1])];
                    hi = [hi[0].max(q[0]), hi[1].max(q[1])];
                }
            }
        }
    }
    (lo[0] <= hi[0]).then_some((lo, hi))
}

fn scale_k(v: &mut Value, key: &str, f: f64) {
    if let Some(k) = v.get(key).and_then(fnum)
        && k.is_finite()
        && k > 0.0
    {
        v.set(key, Value::Num((k * f).clamp(1e-9, 1e9)));
    }
}

/// The shape transformed (None: the result is not a valid shape).
pub fn transform_shape(sh: &Value, xf: &Xform, pivot: P, k: f64) -> Option<Value> {
    if !xf.valid() {
        return None;
    }
    let mut out = sh.clone();
    let pts: Vec<P> = super::shape_pts(sh)
        .into_iter()
        .map(|q| xf.apply(q, pivot, k))
        .collect();
    out.set("pts", pts_value(&pts));
    let f = xf.k_factor();
    if (f - 1.0).abs() > 1e-12 {
        if matches!(kind(sh), "arc" | "free") {
            scale_k(&mut out, "k", f);
        }
        for key in ["tumour", "pattern", "shape"] {
            if let Some(mut t) = out.get(key).cloned() {
                scale_k(&mut t, "k", f);
                out.set(key, t);
            }
        }
        if let Some(mut t) = out.get("text").cloned() {
            scale_k(&mut t, "k", f);
            out.set("text", t);
        }
    }
    clean_shape(&out)
}

/// Moves a shape by `d` (`[beats, keys]`).
pub fn translate_shape(sh: &Value, d: P) -> Option<Value> {
    let mut out = sh.clone();
    let pts: Vec<P> = super::shape_pts(sh)
        .into_iter()
        .map(|q| [q[0] + d[0], q[1] + d[1]])
        .collect();
    out.set("pts", pts_value(&pts));
    clean_shape(&out)
}

/// Transforms several shapes together around their common centre (or
/// `pivot`); shapes that come out before beat 0 are moved right as a group so
/// the earliest point is at beat 0. None: a shape would not survive.
pub fn transform_shapes(
    shapes: &[Value],
    xf: &Xform,
    pivot: Option<P>,
    k: f64,
) -> Option<Vec<Value>> {
    let (lo, hi) = bounds(shapes)?;
    let pivot = pivot.unwrap_or([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]);
    let moved: Vec<Value> = shapes
        .iter()
        .map(|s| transform_shape(s, xf, pivot, k))
        .collect::<Option<_>>()?;
    let (lo2, _) = bounds(&moved)?;
    if lo2[0] < 0.0 {
        return moved
            .iter()
            .map(|s| translate_shape(s, [-lo2[0], 0.0]))
            .collect();
    }
    Some(moved)
}

// ================================================================ box shapes

/// Spiderweb's `box_frame`: origin, end of the time axis, end of the key
/// axis.
pub fn box_frame(a: P, b: P) -> [P; 3] {
    let (b0, b1) = (a[0].min(b[0]), a[0].max(b[0]));
    let (p0, p1) = (a[1].min(b[1]), a[1].max(b[1]));
    [[b0, p0], [b1, p0], [b0, p1]]
}

/// A custom shape in `frame` with `strokes`; `base` gives the velocities.
pub fn custom_shape(frame: [P; 3], strokes: Vec<Value>, name: &str, base: &Value) -> Value {
    let mut sh = Value::obj();
    for key in ["vel0", "vel1"] {
        sh.set(key, base.get(key).cloned().unwrap_or(Value::Num(127.0)));
    }
    sh.set("kind", Value::Str("custom".into()));
    sh.set("pts", pts_value(&frame));
    sh.set("name", Value::Str(name.into()));
    sh.set("strokes", Value::Arr(strokes));
    sh.set("fill", Value::Str("empty".into()));
    sh
}

/// The ellipse stroke filling the whole box.
pub fn ellipse_stroke() -> Value {
    let mut st = Value::obj();
    st.set("kind", Value::Str("ellipse".into()));
    st.set(
        "box",
        Value::Arr([0.0, 0.0, 1.0, 1.0].into_iter().map(Value::Num).collect()),
    );
    st
}

/// A circle (ellipse) in the box from `a` to `b`.
pub fn circle_shape(a: P, b: P, base: &Value) -> Option<Value> {
    clean_shape(&custom_shape(
        box_frame(a, b),
        vec![ellipse_stroke()],
        "Circle",
        base,
    ))
}

/// A square (rectangle) in the box from `a` to `b`.
pub fn square_shape(a: P, b: P, base: &Value) -> Option<Value> {
    clean_shape(&custom_shape(
        box_frame(a, b),
        vec![box_stroke()],
        "Square",
        base,
    ))
}

/// A polygon / star / crossing star (`settings` = Spiderweb's polygon
/// dict: points, style, inner, skip) in the box from `a` to `b`.
pub fn polygon_shape(a: P, b: P, settings: &Value, base: &Value) -> Option<Value> {
    let pg = polygon::clean_polygon(Some(settings))?;
    let name = match gets(&pg, "style") {
        Some("star") => "Star",
        Some("cross") => "Crossing star",
        _ => "Polygon",
    };
    let mut sh = custom_shape(box_frame(a, b), polygon::polygon_strokes(&pg), name, base);
    sh.set("polygon", pg);
    clean_shape(&sh)
}

/// An Hz bass box: spam whose gate is one wave of the tone `key` + `cents`
/// at `bpm` (Spiderweb's H tool).
pub fn hz_shape(a: P, b: P, key: i64, cents: f64, bpm: f64, base: &Value) -> Option<Value> {
    let mut sh = custom_shape(box_frame(a, b), vec![box_stroke()], "Hz bass", base);
    let mut hz = Value::obj();
    hz.set("key", Value::Num(key.clamp(0, 255) as f64));
    hz.set("cents", Value::Num(cents.clamp(-1200.0, 1200.0)));
    hz.set("bpm", Value::Num(bpm));
    sh.set("hz", hz);
    sh.set("fill", Value::Str("spam".into()));
    sh.set("gate", Value::Num(hzbass::hz_gate(key as f64, cents, bpm)));
    clean_shape(&sh)
}

/// Notes taken from the project as one shape (Spiderweb's pasted notes):
/// `(tick, length, key, velocity, colour)`; the box covers them exactly.
pub fn notes_shape(notes: &[[i64; 5]], ppq: u32, name: &str) -> Option<Value> {
    let notes: Vec<[i64; 5]> = notes
        .iter()
        .copied()
        // Any key a project can hold (0–255 in 256-Key Mode); the shape
        // stores rows relative to its lowest key.
        .filter(|n| {
            n[0] >= 0
                && n[1] > 0
                && (0..=i64::from(crate::model::note::MAX_PITCH_256)).contains(&n[2])
        })
        .collect();
    if notes.is_empty() {
        return None;
    }
    let ppq = f64::from(ppq.max(1));
    let t0 = notes.iter().map(|n| n[0]).min()?;
    let k0 = notes.iter().map(|n| n[2]).min()?;
    let rows: Vec<[i64; 5]> = notes
        .iter()
        .map(|n| {
            [
                n[0] - t0,
                n[0] - t0 + n[1],
                n[2] - k0,
                n[3].clamp(1, 127),
                n[4].max(0),
            ]
        })
        .collect();
    let end = rows.iter().map(|r| r[1]).max()?;
    let top = rows.iter().map(|r| r[2]).max()?;
    let mean = rows.iter().map(|r| r[3] as f64).sum::<f64>() / rows.len() as f64;
    let v = super::curves::rne(mean).clamp(1.0, 127.0);
    let frame = box_frame(
        [t0 as f64 / ppq, k0 as f64 - 0.5],
        [(t0 + end) as f64 / ppq, k0 as f64 + 0.5 + top as f64],
    );
    let mut sh = custom_shape(frame, vec![box_stroke()], name, &Value::obj());
    sh.set("vel0", Value::Num(v));
    sh.set("vel1", Value::Num(v));
    sh.set("notes", Value::Str(pack_notes(&rows)));
    sh.set("own_vel", Value::Bool(true));
    clean_shape(&sh)
}

// ================================================================ live shapes

/// The frame's (u, v) of a board point (None: flat frame).
fn to_uv(frame: &[P; 3], q: P) -> Option<P> {
    let (o, u, v) = (frame[0], frame[1], frame[2]);
    let (ub, up) = (u[0] - o[0], u[1] - o[1]);
    let (vb, vp) = (v[0] - o[0], v[1] - o[1]);
    let det = ub * vp - up * vb;
    if det.abs() < 1e-12 {
        return None;
    }
    let (db, dp) = (q[0] - o[0], q[1] - o[1]);
    Some([(db * vp - dp * vb) / det, (ub * dp - up * db) / det])
}

fn to_bp(frame: &[P; 3], q: P) -> P {
    let (o, u, v) = (frame[0], frame[1], frame[2]);
    [
        o[0] + q[0] * (u[0] - o[0]) + q[1] * (v[0] - o[0]),
        o[1] + q[0] * (u[1] - o[1]) + q[1] * (v[1] - o[1]),
    ]
}

fn frame_of(sh: &Value) -> Option<[P; 3]> {
    let f = super::shape_pts(sh);
    (f.len() == 3).then(|| [f[0], f[1], f[2]])
}

fn poly_stroke(pts: Vec<P>) -> Value {
    let mut st = Value::obj();
    st.set("kind", Value::Str("poly".into()));
    st.set("pts", pts_value(&pts));
    st
}

/// One shape's strokes in board coordinates, with what kind each is:
/// (kind, points, k for arcs / freehand, smooth, sharp corners).
struct BpStroke {
    kind: &'static str,
    pts: Vec<P>,
    k: f64,
    smooth: f64,
    sharp: Option<Value>,
    src: i64,
}

fn line_strokes(sh: &Value, src: i64) -> Vec<BpStroke> {
    let k = kind(sh);
    let baked = sh
        .get("tumour")
        .is_some_and(|t| t.get("on") != Some(&Value::Bool(false)))
        || pattern::has_formula(sh)
        || joined::is_joined(sh)
        || matches!(k, "funnel" | "custom");
    if baked {
        return shape_strokes(sh)
            .into_iter()
            .filter(|s| s.len() >= 2)
            .map(|pts| BpStroke {
                kind: "poly",
                pts,
                k: 1.0,
                smooth: 0.0,
                sharp: None,
                src,
            })
            .collect();
    }
    let pts = super::shape_pts(sh);
    let st = |kind: &'static str| BpStroke {
        kind,
        pts: pts.clone(),
        k: getf(sh, "k", 1.0),
        smooth: getf(sh, "smooth", 0.0),
        sharp: sh.get("sharp").cloned(),
        src,
    };
    vec![match k {
        "curve" => st("curve"),
        "arc" => st("arc"),
        "free" => st("free"),
        _ => st("poly"),
    }]
}

/// Several shapes → one live (custom) shape whose strokes are theirs, in a
/// box around them; the shapes are kept so "Split" can give them back. The
/// first custom shape among them (if any) gives the fill settings.
pub fn to_live(shapes: &[Value]) -> Option<Value> {
    if shapes.is_empty() {
        return None;
    }
    let mut strokes: Vec<BpStroke> = Vec::new();
    for (i, sh) in shapes.iter().enumerate() {
        strokes.extend(line_strokes(sh, i as i64 + 1));
    }
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for s in &strokes {
        for q in &s.pts {
            lo = [lo[0].min(q[0]), lo[1].min(q[1])];
            hi = [hi[0].max(q[0]), hi[1].max(q[1])];
        }
    }
    if lo[0]
        .partial_cmp(&hi[0])
        .is_none_or(|o| o == std::cmp::Ordering::Greater)
    {
        return None;
    }
    // flat sides get width / height 1, centred (custom.refit)
    if hi[0] - lo[0] < 1e-9 {
        lo[0] -= 0.5;
        hi[0] += 0.5;
    }
    if hi[1] - lo[1] < 1e-9 {
        lo[1] -= 0.5;
        hi[1] += 0.5;
    }
    let frame = box_frame(lo, hi);
    let (w, h) = (hi[0] - lo[0], hi[1] - lo[1]);
    let out_strokes: Vec<Value> = strokes
        .iter()
        .map(|s| {
            let uv: Vec<P> = s
                .pts
                .iter()
                .map(|q| to_uv(&frame, *q).unwrap_or([0.0, 0.0]))
                .collect();
            let mut st = poly_stroke(uv);
            match s.kind {
                "curve" => {
                    st.set("kind", Value::Str("curve".into()));
                    if let Some(sh) = &s.sharp {
                        st.set("sharp", sh.clone());
                    }
                }
                "arc" => {
                    st.set("kind", Value::Str("arc".into()));
                    st.set("k", Value::Num((s.k * h / w).clamp(1e-9, 1e9)));
                }
                "free" => {
                    st.set("free", Value::Bool(true));
                    st.set("smooth", Value::Num(s.smooth));
                    st.set("k", Value::Num((s.k * h / w).clamp(1e-9, 1e9)));
                }
                _ => {}
            }
            st.set("src", Value::Num(s.src as f64));
            st
        })
        .collect();
    let first = &shapes[0];
    let mut sh = custom_shape(frame, out_strokes.clone(), "Live shape", first);
    if let Some(c) = shapes.iter().find(|s| kind(s) == "custom") {
        for key in ["name", "fill", "gate", "align", "ends", "hz"] {
            if let Some(v) = c.get(key) {
                sh.set(key, v.clone());
            }
        }
        for key in super::custom::CUSTOM_FLAGS {
            if let Some(v) = c.get(key) {
                sh.set(key, v.clone());
            }
        }
    }
    let mut from = Value::obj();
    from.set("shapes", Value::Arr(shapes.to_vec()));
    from.set("strokes", Value::Arr(out_strokes));
    from.set("pts", pts_value(&frame));
    sh.set("from", from);
    clean_shape(&sh)
}

fn close(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => (x - y).abs() <= 1e-7 + 1e-9 * x.abs().max(y.abs()),
        (Value::Arr(x), Value::Arr(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| close(p, q))
        }
        (Value::Obj(x), Value::Obj(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && close(v, v2)))
        }
        _ => a == b,
    }
}

/// A live shape's original shapes, moved with it, when its strokes are
/// unchanged and its box only moved (Spiderweb's `originals`).
pub fn originals(sh: &Value) -> Option<Vec<Value>> {
    let from = sh.get("from")?;
    let olds = from.get("shapes")?.as_arr()?;
    let fp = from.get("pts").and_then(pts_of)?;
    let now = frame_of(sh)?;
    if fp.len() != 3 || !close(sh.get("strokes")?, from.get("strokes")?) {
        return None;
    }
    let d = [now[0][0] - fp[0][0], now[0][1] - fp[0][1]];
    for i in 0..3 {
        if (now[i][0] - fp[i][0] - d[0]).abs() > 1e-9 || (now[i][1] - fp[i][1] - d[1]).abs() > 1e-9
        {
            return None;
        }
    }
    olds.iter().map(|o| translate_shape(o, d)).collect()
}

/// A live / custom shape split into one shape per stroke (or its original
/// shapes when it is unchanged). Text and pasted notes cannot be split.
pub fn split(sh: &Value) -> Option<Vec<Value>> {
    if kind(sh) != "custom" || sh.has("text") || sh.has("notes") {
        return None;
    }
    if let Some(o) = originals(sh) {
        return Some(o);
    }
    let frame = frame_of(sh)?;
    let (w, h) = (
        ((frame[1][0] - frame[0][0]).powi(2) + (frame[1][1] - frame[0][1]).powi(2)).sqrt(),
        ((frame[2][0] - frame[0][0]).powi(2) + (frame[2][1] - frame[0][1]).powi(2)).sqrt(),
    );
    let strokes = sh.get("strokes")?.as_arr()?;
    let mut out = Vec::new();
    for st in strokes {
        let mut n = Value::obj();
        for key in ["vel0", "vel1"] {
            n.set(key, sh.get(key).cloned().unwrap_or(Value::Num(127.0)));
        }
        let sk = gets(st, "kind").unwrap_or("poly");
        let uv: Vec<P> = if sk == "ellipse" {
            super::custom::stroke_points(st)
        } else {
            st.get("pts").and_then(pts_of).unwrap_or_default()
        };
        let pts: Vec<P> = uv.iter().map(|q| to_bp(&frame, *q)).collect();
        if pts.len() < 2 {
            continue;
        }
        let kk = getf(st, "k", 1.0) * w.max(1e-9) / h.max(1e-9);
        match sk {
            "curve" => {
                n.set("kind", Value::Str("curve".into()));
                if let Some(s) = st.get("sharp") {
                    n.set("sharp", s.clone());
                }
            }
            "arc" => {
                n.set("kind", Value::Str("arc".into()));
                n.set("k", Value::Num(kk));
            }
            _ if st.get("free") == Some(&Value::Bool(true)) => {
                n.set("kind", Value::Str("free".into()));
                n.set("smooth", Value::Num(getf(st, "smooth", 0.0)));
                n.set("k", Value::Num(kk));
            }
            _ => n.set("kind", Value::Str("poly".into())),
        }
        n.set("pts", pts_value(&pts));
        for key in ["pattern", "shape"] {
            if let Some(v) = st.get(key) {
                n.set(key, v.clone());
            }
        }
        if let Some(c) = clean_shape(&n) {
            out.push(c);
        }
    }
    (!out.is_empty()).then_some(out)
}
