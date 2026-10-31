//! Declarative tool parameters.
//!
//! Every tool describes its controls as a list of [`ParamSpec`]s. The app renders
//! them generically (one modeless window for every tool), presets store them as
//! key/value text, and kernels read them through a [`ParamSet`] whose accessors
//! fall back to the spec's default and clamp to the spec's range — so a preset
//! from a newer version, a hand-edited file or a missing key can never produce an
//! out-of-range value.

use crate::model::project::KeyRange;
use std::borrow::Cow;
use std::collections::BTreeMap;

/// Kind (and range) of one parameter.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
        /// Display unit ("%", "°", "ticks", "semitones", …).
        unit: &'static str,
    },
    Bool,
    Choice(&'static [&'static str]),
    /// A length in ticks (shown with musical names when it matches one).
    Ticks {
        min: u64,
        max: u64,
    },
    /// A pitch (shown as a note name): 0–127, or 0–255 in a 256-Key Mode
    /// project (always keyed, see [`ParamSpec::keyed`]).
    Pitch,
    /// A curve on [0,1] → [0,1] (envelopes, transfer functions, splines).
    Curve,
    /// A list of integers (interval sequences, subchannel cycles, patterns).
    List {
        min: i64,
        max: i64,
        max_len: usize,
    },
    /// The random seed (dice button).
    Seed,
    /// Free text (typed chord progressions, names).
    Text {
        max_len: usize,
    },
}

/// One control of a tool.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub default: ParamValue,
    /// 0 = base control; 1–10 = belongs to that advanced function.
    pub adv: u8,
    pub help: &'static str,
    /// How the range follows the project's key range (v0.9 256-Key Mode,
    /// §058; [`ParamSpec::range_in`], [`ParamSpec::default_in`]).
    pub keyed: Keyed,
    /// B1-0113: a curve whose kernel maps its value into a range and clamps
    /// the result itself may overshoot 0 – 1 ([`Curve::overshoot`]); for
    /// every other curve [`Params::curve`] drops the overshoot.
    pub overshoot: bool,
}

/// How a parameter declared for the standard 0–127 keyboard reads in a
/// 256-Key Mode project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keyed {
    /// The same in every key range.
    #[default]
    No,
    /// Keys or semitones: a bound or default of ±127 (the top key, the whole
    /// keyboard's span) stands for ± the project's top key (±255); every
    /// other value is the same. Pitch parameters are keyed this way.
    Keys,
    /// An octave register: a positive upper / negative lower bound reaches
    /// as many octaves further as the added keys hold (10 for 256 keys).
    Octaves,
}

/// The standard top key ([`crate::model::note::MAX_PITCH`]) as a parameter value.
const TOP: i64 = crate::model::note::MAX_PITCH as i64;

/// A [`Keyed::Keys`] bound / value in the key range `keys`: ±127 becomes ±
/// its top key.
fn keyed_value(v: i64, keys: KeyRange) -> i64 {
    let top = i64::from(keys.max_pitch());
    match v {
        TOP => top,
        v if v == -TOP => -top,
        v => v,
    }
}

/// Whole octaves the key range `keys` adds above the standard keyboard.
fn added_octaves(keys: KeyRange) -> i64 {
    (i64::from(keys.max_pitch()) - TOP) / 12
}

/// A parameter value.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Choice(u32),
    Curve(Curve),
    List(Vec<i64>),
    Text(String),
}

/// What a curve gives outside its first / last point (v0.9 §105). Every
/// point of a curve is free (§104), so the domain [0, 1] may extend past the
/// points on either side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Outside {
    /// The nearest end point's value (the default; what v0.8 did).
    #[default]
    Clamp,
    /// The end segment's slope continued (clamped to the graph's 0–1 range).
    Extend,
    /// The curve between the first and last point repeated.
    Repeat,
    /// The curve mirrored back and forth (ping-pong).
    Reflect,
}

impl Outside {
    pub const ALL: [Outside; 4] = [
        Outside::Clamp,
        Outside::Extend,
        Outside::Repeat,
        Outside::Reflect,
    ];

    /// Shown in the editor.
    pub fn name(self) -> &'static str {
        match self {
            Outside::Clamp => "Clamp",
            Outside::Extend => "Extend",
            Outside::Repeat => "Repeat",
            Outside::Reflect => "Reflect",
        }
    }

    /// Text form (presets).
    pub fn key(self) -> &'static str {
        match self {
            Outside::Clamp => "clamp",
            Outside::Extend => "extend",
            Outside::Repeat => "repeat",
            Outside::Reflect => "reflect",
        }
    }

    pub fn from_key(k: &str) -> Option<Outside> {
        Outside::ALL.into_iter().find(|o| o.key() == k)
    }
}

/// Piecewise curve through points in [0,1]² (v0.9 §103–§108). Every point is
/// free: nothing pins the first / last point to x = 0 / 1; outside the points
/// [`Curve::outside`] decides. [`Curve::smoothness`] (0–1, shown as 0–100 %)
/// blends straight segments (0) with the monotone cubic (Fritsch–Carlson, 1);
/// it changes the path between the points and never moves one.
///
/// v1.0-beta1 (§017): each point may have its own smoothness
/// ([`Curve::local`]: Not Smooth = a corner, Smooth, or a degree between),
/// and the graph may overshoot its nominal 0 – 1 range by
/// [`Curve::overshoot`] (y in `−o … 1 + o`; only for parameters that allow
/// it, [`ParamSpec::overshoot`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    pub points: Vec<(f64, f64)>,
    /// 0 = straight segments, 1 = the monotone cubic (v0.8's "Smooth").
    pub smoothness: f64,
    pub outside: Outside,
    /// B1-0112: each point's own smoothness, parallel to `points` (missing
    /// entries follow [`Curve::smoothness`]): `None` = the graph's,
    /// `Some(0.0)` = Not Smooth (a corner), `Some(1.0)` = Smooth, between a
    /// local degree. Empty for every v0.9 curve.
    pub local: Vec<Option<f64>>,
    /// B1-0113: how far the graph may go beyond its nominal 0 – 1 range, as
    /// a share of it (0 = off, at most [`MAX_OVERSHOOT`]).
    pub overshoot: f64,
}

/// Most points a curve may have (presets are validated against it).
pub const MAX_CURVE_POINTS: usize = 64;

/// The largest overshoot (B1-0113): the graph may reach −2 … 3, five times
/// its nominal range.
pub const MAX_OVERSHOOT: f64 = 2.0;

impl Curve {
    /// Straight segments through `points`, clamped outside them.
    pub fn new(points: Vec<(f64, f64)>) -> Curve {
        Curve {
            points,
            smoothness: 0.0,
            outside: Outside::Clamp,
            local: Vec::new(),
            overshoot: 0.0,
        }
    }

    /// The monotone cubic (Smoothness 100 %) through `points`.
    pub fn smooth(points: Vec<(f64, f64)>) -> Curve {
        Curve::new(points).with_smoothness(1.0)
    }

    pub fn with_smoothness(mut self, s: f64) -> Curve {
        self.smoothness = s;
        self
    }

    pub fn with_outside(mut self, o: Outside) -> Curve {
        self.outside = o;
        self
    }

    /// B1-0112: point `i`'s own smoothness (`None`: the graph's).
    pub fn with_local(mut self, i: usize, s: Option<f64>) -> Curve {
        if self.local.len() <= i {
            self.local.resize(i + 1, None);
        }
        self.local[i] = s;
        self
    }

    /// B1-0113: the overshoot (0 = off).
    pub fn with_overshoot(mut self, o: f64) -> Curve {
        self.overshoot = o;
        self
    }

    /// Point `i`'s smoothness: its own, or the graph's.
    #[inline]
    pub fn local_at(&self, i: usize) -> f64 {
        self.local
            .get(i)
            .copied()
            .flatten()
            .unwrap_or(self.smoothness)
    }

    /// Whether any point has its own smoothness.
    #[inline]
    pub fn has_local(&self) -> bool {
        self.local.iter().any(Option::is_some)
    }

    /// The values the graph may take: 0 – 1, or `−o … 1 + o` with an
    /// overshoot `o` (B1-0113).
    #[inline]
    pub fn y_range(&self) -> (f64, f64) {
        let o = if self.overshoot.is_finite() {
            self.overshoot.clamp(0.0, MAX_OVERSHOOT)
        } else {
            0.0
        };
        (-o, 1.0 + o)
    }

    /// The curve without overshoot: points pinned into 0 – 1 (what a
    /// parameter that does not allow overshoot reads).
    pub fn without_overshoot(&self) -> Curve {
        let mut c = self.clone();
        c.overshoot = 0.0;
        for p in &mut c.points {
            p.1 = p.1.clamp(0.0, 1.0);
        }
        c
    }

    /// y = x.
    pub fn identity() -> Curve {
        Curve::new(vec![(0.0, 0.0), (1.0, 1.0)])
    }

    /// A constant.
    pub fn flat(y: f64) -> Curve {
        Curve::new(vec![(0.0, y), (1.0, y)])
    }

    /// Straight line from `a` (x = 0) to `b` (x = 1).
    pub fn line(a: f64, b: f64) -> Curve {
        Curve::new(vec![(0.0, a), (1.0, b)])
    }

    /// Sorted, clamped to [0,1] (y to the overshoot range), at least two
    /// points, at most [`MAX_CURVE_POINTS`]; NaN replaced by 0 (points and
    /// smoothness); each point keeps its own smoothness (B1-0112). The
    /// points stay where they are: the ends are not moved to x = 0 / 1.
    pub fn sanitized(&self) -> Curve {
        let fix = |v: f64| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let over = if self.overshoot.is_finite() {
            self.overshoot.clamp(0.0, MAX_OVERSHOOT)
        } else {
            0.0
        };
        let fixy = |v: f64| {
            if v.is_finite() {
                v.clamp(-over, 1.0 + over)
            } else {
                0.0
            }
        };
        let mut p: Vec<((f64, f64), Option<f64>)> = self
            .points
            .iter()
            .take(MAX_CURVE_POINTS)
            .enumerate()
            .map(|(i, &(x, y))| {
                let l = self.local.get(i).copied().flatten();
                (
                    (fix(x), fixy(y)),
                    l.map(|s| {
                        if s.is_finite() {
                            s.clamp(0.0, 1.0)
                        } else {
                            0.0
                        }
                    }),
                )
            })
            .collect();
        p.sort_by(|a, b| a.0.0.total_cmp(&b.0.0));
        p.dedup_by(|b, a| (a.0.0 - b.0.0).abs() < 1e-12);
        let mut base = match p.len() {
            0 => Curve::identity(),
            1 => Curve::flat(p[0].0.1),
            _ => Curve::new(p.iter().map(|q| q.0).collect()),
        };
        if p.len() >= 2 && p.iter().any(|q| q.1.is_some()) {
            base.local = p.iter().map(|q| q.1).collect();
        }
        base.overshoot = over;
        base.with_smoothness(fix(self.smoothness))
            .with_outside(self.outside)
    }

    /// Value at `x`: on the points exactly their y; between them the path
    /// [`Curve::smoothness`] selects; outside them [`Curve::outside`].
    /// Allocation-free, O(log points).
    #[inline]
    pub fn eval(&self, x: f64) -> f64 {
        let p = &self.points;
        if p.is_empty() {
            return x.clamp(0.0, 1.0);
        }
        let x = if x.is_finite() { x } else { 0.0 };
        let first = p[0];
        let last = p[p.len() - 1];
        if x <= first.0 || x >= last.0 {
            // Strictly outside: the policy (an end point itself is exact).
            if self.outside != Outside::Clamp && (x < first.0 || x > last.0) && p.len() >= 2 {
                return self.eval_outside(x);
            }
            return if x <= first.0 { first.1 } else { last.1 };
        }
        self.eval_inside(x)
    }

    /// Extend / Repeat / Reflect at `x` strictly outside the points (kept
    /// out of line: the common Clamp path stays small).
    #[inline(never)]
    fn eval_outside(&self, x: f64) -> f64 {
        let p = &self.points;
        let (first, last) = (p[0], p[p.len() - 1]);
        if self.outside == Outside::Extend {
            let (y, m, d) = if x < first.0 {
                (first.1, self.slope(0), x - first.0)
            } else {
                (last.1, self.slope(p.len() - 2), x - last.0)
            };
            let (lo, hi) = self.y_range();
            return (y + m * d).clamp(lo, hi);
        }
        let x = self.fold(x);
        if x <= first.0 {
            return first.1;
        }
        if x >= last.0 {
            return last.1;
        }
        self.eval_inside(x)
    }

    /// Value strictly between the first and last point.
    #[inline(always)]
    fn eval_inside(&self, x: f64) -> f64 {
        let p = &self.points;
        // Segment containing x.
        let i = match p.binary_search_by(|q| q.0.total_cmp(&x)) {
            Ok(i) => return p[i].1,
            Err(i) => i - 1,
        };
        let (x0, y0) = p[i];
        let (x1, y1) = p[i + 1];
        let h = x1 - x0;
        if h <= 0.0 {
            return y1;
        }
        let t = (x - x0) / h;
        if !self.local.is_empty() && self.has_local() {
            return self.eval_local(i, t);
        }
        let s = self.smoothness;
        // NaN or 0 % (and two points): straight.
        if s > 0.0 && p.len() >= 3 {
            // Monotone cubic Hermite.
            let m0 = self.tangent(i);
            let m1 = self.tangent(i + 1);
            let t2 = t * t;
            let t3 = t2 * t;
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            let (lo, hi) = self.y_range();
            let cubic = (h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1).clamp(lo, hi);
            if s >= 1.0 {
                return cubic;
            }
            // Both paths run monotonically from y0 to y1 inside the segment,
            // so their blend does too and meets the points exactly.
            let lin = y0 + (y1 - y0) * t;
            return lin + (cubic - lin) * s;
        }
        y0 + (y1 - y0) * t
    }

    /// B1-0112: segment `i` at `t` (0 – 1) with each end's own smoothness:
    /// the Hermite segment whose end tangents are `sec + s·(m − sec)` (the
    /// secant `sec`, the monotone tangent `m`, the point's smoothness `s`).
    /// A point at 0 is a corner (both segments leave it along their own
    /// secants); with every point at the graph's smoothness this is the same
    /// path as the global blend. Monotone between the points, so it stays
    /// between their values and never moves one.
    #[inline(never)]
    fn eval_local(&self, i: usize, t: f64) -> f64 {
        let p = &self.points;
        let (x0, y0) = p[i];
        let (x1, y1) = p[i + 1];
        let h = x1 - x0;
        let lin = y0 + (y1 - y0) * t;
        let (s0, s1) = (self.local_at(i), self.local_at(i + 1));
        let (s0, s1) = (
            if s0.is_finite() {
                s0.clamp(0.0, 1.0)
            } else {
                0.0
            },
            if s1.is_finite() {
                s1.clamp(0.0, 1.0)
            } else {
                0.0
            },
        );
        if p.len() < 3 || (s0 <= 0.0 && s1 <= 0.0) {
            return lin;
        }
        let sec = (y1 - y0) / h;
        let m0 = sec + s0 * (self.tangent(i) - sec);
        let m1 = sec + s1 * (self.tangent(i + 1) - sec);
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        let (lo, hi) = self.y_range();
        (h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1).clamp(lo, hi)
    }

    /// `x` outside the points folded into [first, last] (Repeat / Reflect).
    fn fold(&self, x: f64) -> f64 {
        let x0 = self.points[0].0;
        let span = self.points[self.points.len() - 1].0 - x0;
        if span <= 0.0 {
            return x0;
        }
        let u = (x - x0).rem_euclid(if self.outside == Outside::Reflect {
            2.0 * span
        } else {
            span
        });
        if u > span {
            x0 + (2.0 * span - u)
        } else {
            x0 + u
        }
    }

    fn slope(&self, i: usize) -> f64 {
        let (x0, y0) = self.points[i];
        let (x1, y1) = self.points[i + 1];
        if x1 > x0 { (y1 - y0) / (x1 - x0) } else { 0.0 }
    }

    fn tangent(&self, i: usize) -> f64 {
        let n = self.points.len();
        if i == 0 {
            return self.slope(0);
        }
        if i == n - 1 {
            return self.slope(n - 2);
        }
        let a = self.slope(i - 1);
        let b = self.slope(i);
        if a * b <= 0.0 {
            0.0
        } else {
            // Harmonic mean keeps the interpolant monotone.
            2.0 * a * b / (a + b)
        }
    }

    /// Text form: `s[@outside][^o];x,y[,m];x,y[,m]…`, full precision. `s` is the
    /// smoothness 0–1 (v0.8's smooth flag `0` / `1` reads as 0 / 100 %);
    /// `@outside` is written only when it is not Clamp, `^o` only with an
    /// overshoot (B1-0113) and `,m` only for a point with its own smoothness
    /// (B1-0112), so v0.8 / v0.9 curves keep their text.
    pub fn to_text(&self) -> String {
        let s = self.smoothness;
        let mut t = if s <= 0.0 || !s.is_finite() {
            String::from("0")
        } else if s >= 1.0 {
            String::from("1")
        } else {
            format!("{s}")
        };
        if self.outside != Outside::Clamp {
            t.push('@');
            t.push_str(self.outside.key());
        }
        if self.overshoot > 0.0 && self.overshoot.is_finite() {
            t.push_str(&format!("^{}", self.overshoot));
        }
        for (i, (x, y)) in self.points.iter().enumerate() {
            // Preserve legacy bytes when six decimals were already lossless.
            let number = |n: f64| {
                let old = format!("{n:.6}");
                if old.parse::<f64>().is_ok_and(|v| v.to_bits() == n.to_bits()) {
                    old
                } else {
                    n.to_string()
                }
            };
            t.push_str(&format!(";{},{}", number(*x), number(*y)));
            if let Some(m) = self.local.get(i).copied().flatten() {
                t.push_str(&format!(",{m}"));
            }
        }
        t
    }

    pub fn from_text(t: &str) -> Option<Curve> {
        let mut it = t.split(';');
        let head = it.next()?.trim();
        let (head, over) = match head.split_once('^') {
            Some((a, b)) => (a.trim(), Some(b.trim())),
            None => (head, None),
        };
        let (sm, out) = match head.split_once('@') {
            Some((a, b)) => (a.trim(), Some(b.trim())),
            None => (head, None),
        };
        let smoothness: f64 = sm.parse().ok()?;
        if !(0.0..=1.0).contains(&smoothness) {
            return None;
        }
        let outside = match out {
            None => Outside::Clamp,
            Some(k) => Outside::from_key(k)?,
        };
        let overshoot = match over {
            None => 0.0,
            Some(o) => {
                let o: f64 = o.parse().ok()?;
                if !(0.0..=MAX_OVERSHOOT).contains(&o) {
                    return None;
                }
                o
            }
        };
        let mut points = Vec::new();
        let mut local = Vec::new();
        for part in it {
            if points.len() >= MAX_CURVE_POINTS {
                return None;
            }
            let mut f = part.split(',');
            let x: f64 = f.next()?.trim().parse().ok()?;
            let y: f64 = f.next()?.trim().parse().ok()?;
            let m: Option<f64> = match f.next() {
                Some(m) => Some(m.trim().parse().ok()?),
                None => None,
            };
            if f.next().is_some()
                || !x.is_finite()
                || !y.is_finite()
                || m.is_some_and(|m| !(0.0..=1.0).contains(&m))
            {
                return None;
            }
            points.push((x, y));
            local.push(m);
        }
        let mut c = Curve::new(points)
            .with_smoothness(smoothness)
            .with_outside(outside)
            .with_overshoot(overshoot);
        c.local = local;
        Some(c.sanitized())
    }
}

impl ParamValue {
    /// Text form used by presets (one line, no newlines).
    pub fn to_text(&self) -> String {
        match self {
            ParamValue::Int(v) => v.to_string(),
            ParamValue::Float(v) => format!("{v}"),
            ParamValue::Bool(b) => (if *b { "1" } else { "0" }).into(),
            ParamValue::Choice(c) => c.to_string(),
            ParamValue::Curve(c) => c.to_text(),
            ParamValue::List(l) => l
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(","),
            ParamValue::Text(t) => t.replace(['\n', '\r'], " "),
        }
    }

    /// Parses `text` as a value of `kind`, validated (None when invalid).
    pub fn parse(kind: &ParamKind, text: &str) -> Option<ParamValue> {
        let t = text.trim();
        let v = match kind {
            ParamKind::Int { .. } | ParamKind::Ticks { .. } | ParamKind::Pitch => {
                ParamValue::Int(t.parse().ok()?)
            }
            ParamKind::Seed => ParamValue::Int(t.parse::<u64>().ok()? as i64),
            ParamKind::Float { .. } => {
                let f: f64 = t.parse().ok()?;
                if !f.is_finite() {
                    return None;
                }
                ParamValue::Float(f)
            }
            ParamKind::Bool => ParamValue::Bool(match t {
                "1" | "true" => true,
                "0" | "false" => false,
                _ => return None,
            }),
            ParamKind::Choice(opts) => {
                let c: u32 = t.parse().ok()?;
                if (c as usize) >= opts.len() {
                    return None;
                }
                ParamValue::Choice(c)
            }
            ParamKind::Curve => ParamValue::Curve(Curve::from_text(t)?),
            ParamKind::List { max_len, .. } => ParamValue::List(parse_list(t, *max_len)?),
            ParamKind::Text { max_len } => {
                if t.chars().count() > *max_len {
                    return None;
                }
                ParamValue::Text(t.to_string())
            }
        };
        Some(v)
    }
}

/// Parses "1, -2, 3" (also spaces / semicolons) into at most `max_len` integers.
pub fn parse_list(t: &str, max_len: usize) -> Option<Vec<i64>> {
    let mut out = Vec::new();
    for part in t.split([',', ' ', ';']).filter(|s| !s.trim().is_empty()) {
        if out.len() >= max_len {
            return None;
        }
        out.push(part.trim().parse().ok()?);
    }
    Some(out)
}

/// The values chosen for a tool: keyed by [`ParamSpec::key`]; missing keys mean
/// the default. Accessors validate against the specs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParamSet {
    values: BTreeMap<String, ParamValue>,
}

impl ParamSet {
    pub fn new() -> Self {
        ParamSet::default()
    }

    /// All defaults of `specs`.
    pub fn defaults(specs: &[ParamSpec]) -> ParamSet {
        let mut p = ParamSet::new();
        for s in specs {
            p.values.insert(s.key.to_string(), s.default.clone());
        }
        p
    }

    /// All defaults of `specs` in a project of key range `keys` (keyed
    /// defaults of 127 are the range's top key, [`ParamSpec::default_in`]).
    pub fn defaults_in(specs: &[ParamSpec], keys: KeyRange) -> ParamSet {
        let mut p = ParamSet::new();
        for s in specs {
            p.values
                .insert(s.key.to_string(), s.default_in(keys).into_owned());
        }
        p
    }

    pub fn set(&mut self, key: &str, v: ParamValue) {
        self.values.insert(key.to_string(), v);
    }

    pub fn get_raw(&self, key: &str) -> Option<&ParamValue> {
        self.values.get(key)
    }

    /// Removes `key` (B2-F01: Reset Order removes the order).
    pub fn remove(&mut self, key: &str) -> Option<ParamValue> {
        self.values.remove(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ParamValue)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// A stable text key of every value (preview cache key, determinism tests).
    pub fn fingerprint(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.values {
            s.push_str(k);
            s.push('=');
            s.push_str(&v.to_text());
            s.push('\n');
        }
        s
    }
}

/// Typed access to a [`ParamSet`] through its specs (defaults + clamping).
pub struct Params<'a> {
    pub specs: &'a [ParamSpec],
    pub set: &'a ParamSet,
    /// The project's key range: keyed parameters ([`ParamSpec::keyed`]) are
    /// validated against it (standard unless [`Params::with_keys`]).
    pub keys: KeyRange,
}

impl<'a> Params<'a> {
    pub fn new(specs: &'a [ParamSpec], set: &'a ParamSet) -> Self {
        Params {
            specs,
            set,
            keys: KeyRange::Standard,
        }
    }

    /// The same values read in a project of key range `keys`.
    pub fn with_keys(self, keys: KeyRange) -> Self {
        Params { keys, ..self }
    }

    /// v1.0-beta2 B2-F01: the members of a reorderable group (declared
    /// order) in the order the values ask for (`order` key; none: declared).
    pub fn sequence(&self, members: &'static [&'static str]) -> Vec<&'static str> {
        super::order::sequence_in(self.set, members)
    }

    fn spec(&self, key: &str) -> Option<&ParamSpec> {
        self.specs.iter().find(|s| s.key == key)
    }

    fn value(&self, key: &str) -> Option<Cow<'_, ParamValue>> {
        match self.set.get_raw(key) {
            Some(v) => Some(Cow::Borrowed(v)),
            None => self.spec(key).map(|s| s.default_in(self.keys)),
        }
    }

    pub fn int(&self, key: &str) -> i64 {
        let raw = match self.value(key).as_deref() {
            Some(ParamValue::Int(v)) => *v,
            Some(ParamValue::Choice(c)) => i64::from(*c),
            Some(ParamValue::Float(f)) => *f as i64,
            Some(ParamValue::Bool(b)) => i64::from(*b),
            _ => 0,
        };
        let Some(spec) = self.spec(key) else {
            return raw;
        };
        match (&spec.kind, spec.range_in(self.keys)) {
            (ParamKind::Ticks { min, max }, _) => {
                raw.clamp(*min as i64, (*max).min(i64::MAX as u64) as i64)
            }
            (_, Some((min, max))) => raw.clamp(min, max),
            _ => raw,
        }
    }

    pub fn ticks(&self, key: &str) -> u64 {
        self.int(key).max(0) as u64
    }

    pub fn seed(&self, key: &str) -> u64 {
        match self.value(key).as_deref() {
            Some(ParamValue::Int(v)) => *v as u64,
            _ => 0,
        }
    }

    pub fn float(&self, key: &str) -> f64 {
        let raw = match self.value(key).as_deref() {
            Some(ParamValue::Float(v)) => *v,
            Some(ParamValue::Int(v)) => *v as f64,
            _ => 0.0,
        };
        let raw = if raw.is_finite() { raw } else { 0.0 };
        match self.spec(key).map(|s| &s.kind) {
            Some(ParamKind::Float { min, max, .. }) => raw.clamp(*min, *max),
            _ => raw,
        }
    }

    pub fn flag(&self, key: &str) -> bool {
        matches!(self.value(key).as_deref(), Some(ParamValue::Bool(true)))
    }

    pub fn choice(&self, key: &str) -> usize {
        let raw = match self.value(key).as_deref() {
            Some(ParamValue::Choice(c)) => *c as usize,
            Some(ParamValue::Int(v)) => (*v).max(0) as usize,
            _ => 0,
        };
        match self.spec(key).map(|s| &s.kind) {
            Some(ParamKind::Choice(o)) => raw.min(o.len().saturating_sub(1)),
            _ => raw,
        }
    }

    /// The curve, sanitized; its overshoot only when the parameter allows
    /// it ([`ParamSpec::overshoot`], B1-0113) — every other kernel sees 0 – 1.
    pub fn curve(&self, key: &str) -> Curve {
        let c = match self.value(key).as_deref() {
            Some(ParamValue::Curve(c)) => c.sanitized(),
            _ => Curve::identity(),
        };
        if c.overshoot > 0.0 && !self.spec(key).is_some_and(|s| s.overshoot) {
            c.without_overshoot()
        } else {
            c
        }
    }

    pub fn list(&self, key: &str) -> Vec<i64> {
        let (min, max, max_len) = match self.spec(key) {
            Some(
                s @ ParamSpec {
                    kind: ParamKind::List { max_len, .. },
                    ..
                },
            ) => {
                let (min, max) = s.range_in(self.keys).unwrap_or((i64::MIN, i64::MAX));
                (min, max, *max_len)
            }
            _ => (i64::MIN, i64::MAX, 4096),
        };
        match self.value(key).as_deref() {
            Some(ParamValue::List(l)) => l
                .iter()
                .take(max_len)
                .map(|v| (*v).clamp(min, max))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn text(&self, key: &str) -> String {
        match self.value(key).as_deref() {
            Some(ParamValue::Text(t)) => t.clone(),
            _ => String::new(),
        }
    }

    /// Whether advanced function `n` (1–10) is switched on (`a{n}` flag).
    pub fn adv(&self, n: u8) -> bool {
        let n = usize::from(n.max(1));
        n <= ADV_KEYS.len() && self.flag(ADV_KEYS[n - 1])
    }
}

/// Keys of the "advanced function enabled" switches (v0.7 tools use the
/// first ten; v0.8 Paint uses fourteen).
pub const ADV_KEYS: [&str; 16] = [
    "a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8", "a9", "a10", "a11", "a12", "a13", "a14", "a15",
    "a16",
];

// ------------------------------------------------------------ spec builders
//
// Short constructors keep the ~60 tool definitions readable.

pub const fn int(
    key: &'static str,
    label: &'static str,
    min: i64,
    max: i64,
    def: i64,
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Int { min, max },
        default: ParamValue::Int(def),
        adv: 0,
        help: "",
        keyed: Keyed::No,
        overshoot: false,
    }
}

pub const fn float(
    key: &'static str,
    label: &'static str,
    min: f64,
    max: f64,
    def: f64,
    unit: &'static str,
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Float { min, max, unit },
        default: ParamValue::Float(def),
        adv: 0,
        help: "",
        keyed: Keyed::No,
        overshoot: false,
    }
}

pub const fn flag(key: &'static str, label: &'static str, def: bool) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Bool,
        default: ParamValue::Bool(def),
        adv: 0,
        help: "",
        keyed: Keyed::No,
        overshoot: false,
    }
}

pub const fn choice(
    key: &'static str,
    label: &'static str,
    options: &'static [&'static str],
    def: u32,
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Choice(options),
        default: ParamValue::Choice(def),
        adv: 0,
        help: "",
        keyed: Keyed::No,
        overshoot: false,
    }
}

pub const fn ticks(
    key: &'static str,
    label: &'static str,
    min: u64,
    max: u64,
    def: i64,
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Ticks { min, max },
        default: ParamValue::Int(def),
        adv: 0,
        help: "",
        keyed: Keyed::No,
        overshoot: false,
    }
}

pub const fn pitch(key: &'static str, label: &'static str, def: i64) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Pitch,
        default: ParamValue::Int(def),
        adv: 0,
        help: "",
        keyed: Keyed::Keys,
        overshoot: false,
    }
}

/// A help text built at run time (Control Sweep targets, B1-0066) as a
/// `&'static str`: each distinct text is stored once for the program's life
/// (a few hundred at most), so building specs again never grows memory.
pub fn intern(text: String) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static TEXTS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut set = TEXTS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(&t) = set.get(text.as_str()) {
        return t;
    }
    let t: &'static str = Box::leak(text.into_boxed_str());
    set.insert(t);
    t
}

pub const fn seed(key: &'static str) -> ParamSpec {
    ParamSpec {
        key,
        label: "Seed",
        kind: ParamKind::Seed,
        default: ParamValue::Int(1),
        adv: 0,
        help: "Where the random choices start: the same seed always gives the same result; another seed gives another variation.",
        keyed: Keyed::No,
        overshoot: false,
    }
}

pub fn curve(key: &'static str, label: &'static str, def: Curve) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Curve,
        default: ParamValue::Curve(def),
        adv: 0,
        help: "",
        keyed: Keyed::No,
        overshoot: false,
    }
}

pub fn list(
    key: &'static str,
    label: &'static str,
    min: i64,
    max: i64,
    max_len: usize,
    def: &[i64],
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::List { min, max, max_len },
        default: ParamValue::List(def.to_vec()),
        adv: 0,
        help: "",
        keyed: Keyed::No,
        overshoot: false,
    }
}

pub fn text(key: &'static str, label: &'static str, max_len: usize, def: &str) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Text { max_len },
        default: ParamValue::Text(def.to_string()),
        adv: 0,
        help: "",
        keyed: Keyed::No,
        overshoot: false,
    }
}

/// The on/off switch of advanced function `n`.
pub fn adv_switch(n: u8, label: &'static str) -> ParamSpec {
    ParamSpec {
        key: ADV_KEYS[usize::from(n) - 1],
        label,
        kind: ParamKind::Bool,
        default: ParamValue::Bool(false),
        adv: n,
        help: "",
        keyed: Keyed::No,
        overshoot: false,
    }
}

/// B1-0066: gives each listed control of a tool's parameter list its help
/// text (the tooltip): `(key, sentence)` pairs written next to the tool's
/// spec. A key that names no control is a stale table (debug builds and
/// the tests that build every spec panic on it).
pub fn with_help(params: &mut [ParamSpec], table: &[(&str, &'static str)]) {
    for &(key, h) in table {
        match params.iter_mut().find(|p| p.key == key) {
            Some(p) => p.help = h,
            None => debug_assert!(false, "help for a control that does not exist: {key}"),
        }
    }
}

impl ParamSpec {
    /// Marks the spec as belonging to advanced function `n`.
    pub fn adv(mut self, n: u8) -> ParamSpec {
        self.adv = n;
        self
    }

    pub fn help(mut self, h: &'static str) -> ParamSpec {
        self.help = h;
        self
    }

    /// Marks the values as keys / semitones of the standard keyboard
    /// ([`Keyed::Keys`]).
    /// B1-0113: this curve may overshoot its nominal 0 – 1 range (the
    /// kernel maps the value into its range and clamps the result).
    pub fn overshoot(mut self) -> ParamSpec {
        debug_assert!(matches!(self.kind, ParamKind::Curve));
        self.overshoot = true;
        self
    }

    pub fn keyed(mut self) -> ParamSpec {
        self.keyed = Keyed::Keys;
        self
    }

    /// Marks the value as an octave register of the standard keyboard
    /// ([`Keyed::Octaves`]).
    pub fn keyed_octaves(mut self) -> ParamSpec {
        self.keyed = Keyed::Octaves;
        self
    }

    /// The valid range of an integer, pitch or list parameter in a project
    /// of key range `keys` (`None` for other kinds; ticks keep their range).
    pub fn range_in(&self, keys: KeyRange) -> Option<(i64, i64)> {
        let (min, max) = match self.kind {
            ParamKind::Int { min, max } | ParamKind::List { min, max, .. } => (min, max),
            ParamKind::Pitch => (0, TOP),
            _ => return None,
        };
        Some(match self.keyed {
            Keyed::No => (min, max),
            Keyed::Keys => (keyed_value(min, keys), keyed_value(max, keys)),
            Keyed::Octaves => {
                let k = added_octaves(keys);
                (
                    if min < 0 { min - k } else { min },
                    if max > 0 { max + k } else { max },
                )
            }
        })
    }

    /// The default in a project of key range `keys`: a keyed default of
    /// ±127 is ± the range's top key (the same value in a standard project).
    pub fn default_in(&self, keys: KeyRange) -> Cow<'_, ParamValue> {
        if self.keyed != Keyed::Keys || keys == KeyRange::Standard {
            return Cow::Borrowed(&self.default);
        }
        match &self.default {
            ParamValue::Int(v) => Cow::Owned(ParamValue::Int(keyed_value(*v, keys))),
            ParamValue::List(l) => Cow::Owned(ParamValue::List(
                l.iter().map(|&v| keyed_value(v, keys)).collect(),
            )),
            v => Cow::Borrowed(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_interpolate_monotonically_and_roundtrip_as_text() {
        let c = Curve::smooth(vec![(0.0, 0.0), (0.3, 0.6), (1.0, 1.0)]);
        let mut prev = -1.0;
        for i in 0..=100 {
            let y = c.eval(i as f64 / 100.0);
            assert!(y >= prev - 1e-12, "monotone");
            prev = y;
        }
        assert!((c.eval(0.3) - 0.6).abs() < 1e-9);
        let t = c.to_text();
        assert_eq!(Curve::from_text(&t).unwrap().points.len(), 3);
        assert!(Curve::from_text("2;0,0").is_none());
        assert!(Curve::from_text("0;nan,1").is_none());
    }

    #[test]
    fn accessors_fall_back_to_defaults_and_clamp() {
        let specs = vec![
            int("n", "N", 1, 8, 3),
            float("f", "F", -1.0, 1.0, 0.5, ""),
            choice("c", "C", &["a", "b"], 1),
        ];
        let mut set = ParamSet::new();
        let p = Params::new(&specs, &set);
        assert_eq!(p.int("n"), 3);
        assert_eq!(p.choice("c"), 1);
        set.set("n", ParamValue::Int(99));
        set.set("f", ParamValue::Float(f64::NAN));
        set.set("c", ParamValue::Choice(7));
        let p = Params::new(&specs, &set);
        assert_eq!(p.int("n"), 8);
        assert_eq!(p.float("f"), 0.0);
        assert_eq!(p.choice("c"), 1);
    }

    #[test]
    fn values_parse_only_when_valid() {
        assert_eq!(
            ParamValue::parse(&ParamKind::Choice(&["a", "b"]), "1"),
            Some(ParamValue::Choice(1))
        );
        assert_eq!(
            ParamValue::parse(&ParamKind::Choice(&["a", "b"]), "2"),
            None
        );
        assert_eq!(
            ParamValue::parse(
                &ParamKind::Float {
                    min: 0.0,
                    max: 1.0,
                    unit: ""
                },
                "inf"
            ),
            None
        );
        assert_eq!(
            ParamValue::parse(
                &ParamKind::List {
                    min: 0,
                    max: 9,
                    max_len: 2
                },
                "1,2,3"
            ),
            None
        );
        assert_eq!(parse_list("0, 4 7", 8), Some(vec![0, 4, 7]));
    }

    // ---------------------------------------------------------- v0.9 S29

    /// v0.8's `Curve::eval`, frozen: the reference every default curve must
    /// still match bit for bit (smooth = Smoothness 100 %, Clamp outside).
    fn v08_eval(p: &[(f64, f64)], smooth: bool, x: f64) -> f64 {
        if p.is_empty() {
            return x.clamp(0.0, 1.0);
        }
        let x = if x.is_finite() { x } else { 0.0 };
        if x <= p[0].0 {
            return p[0].1;
        }
        let last = p[p.len() - 1];
        if x >= last.0 {
            return last.1;
        }
        let i = match p.binary_search_by(|q| q.0.total_cmp(&x)) {
            Ok(i) => return p[i].1,
            Err(i) => i - 1,
        };
        let (x0, y0) = p[i];
        let (x1, y1) = p[i + 1];
        let h = x1 - x0;
        if h <= 0.0 {
            return y1;
        }
        let t = (x - x0) / h;
        if !smooth || p.len() < 3 {
            return y0 + (y1 - y0) * t;
        }
        let slope = |i: usize| {
            let (x0, y0) = p[i];
            let (x1, y1) = p[i + 1];
            if x1 > x0 { (y1 - y0) / (x1 - x0) } else { 0.0 }
        };
        let tangent = |i: usize| {
            let n = p.len();
            if i == 0 {
                return slope(0);
            }
            if i == n - 1 {
                return slope(n - 2);
            }
            let (a, b) = (slope(i - 1), slope(i));
            if a * b <= 0.0 {
                0.0
            } else {
                2.0 * a * b / (a + b)
            }
        };
        let (m0, m1) = (tangent(i), tangent(i + 1));
        let (t2, t3) = (t * t, t * t * t);
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        (h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1).clamp(0.0, 1.0)
    }

    fn xs() -> impl Iterator<Item = f64> {
        // Past both ends, every 1/512 and the points' own x.
        (-256..=768).map(|k| k as f64 / 512.0)
    }

    #[test]
    fn every_tool_default_curve_evaluates_and_writes_exactly_as_in_v08() {
        let mut n = 0;
        for t in crate::transform::registry::registry() {
            for p in (t.spec)().params {
                let ParamValue::Curve(c) = &p.default else {
                    continue;
                };
                n += 1;
                assert!(
                    c.smoothness == 0.0 || c.smoothness == 1.0,
                    "{}.{}: v0.8 defaults are straight or smooth",
                    t.id,
                    p.key
                );
                assert_eq!(c.outside, Outside::Clamp, "{}.{}", t.id, p.key);
                let s = c.sanitized();
                assert_eq!(
                    s.points, c.points,
                    "{}.{}: sanitizing moved a point",
                    t.id, p.key
                );
                let smooth = c.smoothness == 1.0;
                for x in xs().chain(c.points.iter().map(|q| q.0)) {
                    assert_eq!(
                        s.eval(x).to_bits(),
                        v08_eval(&c.points, smooth, x).to_bits(),
                        "{}.{} at x = {x}",
                        t.id,
                        p.key
                    );
                }
                // The text is v0.8's: `0` / `1` and the points.
                let mut v08 = String::from(if smooth { "1" } else { "0" });
                for (x, y) in &c.points {
                    v08.push_str(&format!(";{x:.6},{y:.6}"));
                }
                assert_eq!(c.to_text(), v08, "{}.{}", t.id, p.key);
            }
        }
        assert!(n >= 8, "only {n} curve defaults found");
    }

    #[test]
    fn curve_ends_stay_where_they_are_put_through_sanitize_text_and_params() {
        // §104 / §251: first moved away from x = 0, last moved inward.
        let c = Curve::smooth(vec![(0.2, 0.3), (0.5, 0.9), (0.8, 0.1)]);
        assert_eq!(c.sanitized().points, c.points);
        let back = Curve::from_text(&c.to_text()).unwrap();
        assert_eq!(back.points, c.points);
        assert_eq!(back.smoothness, 1.0);
        let specs = vec![curve("k", "K", Curve::identity())];
        let mut set = ParamSet::new();
        set.set("k", ParamValue::Curve(c.clone()));
        assert_eq!(Params::new(&specs, &set).curve("k").points, c.points);
        // Two points are enough, anywhere.
        let two = Curve::new(vec![(0.9, 0.0), (0.1, 1.0)]).sanitized();
        assert_eq!(two.points, vec![(0.1, 1.0), (0.9, 0.0)]);
        // The points are hit exactly at every smoothness.
        for s in [0.0, 0.37, 0.5, 1.0] {
            let c = c.clone().with_smoothness(s);
            for &(x, y) in &c.points {
                assert_eq!(c.eval(x), y, "smoothness {s} at {x}");
            }
        }
    }

    #[test]
    fn outside_policies_clamp_extend_repeat_and_reflect() {
        // §105: points from 0.25 to 0.75, evaluated past both ends.
        let pts = vec![(0.25, 0.2), (0.5, 0.6), (0.75, 0.4)];
        let c = |o| Curve::new(pts.clone()).with_outside(o);
        let clamp = c(Outside::Clamp);
        assert_eq!(clamp.eval(0.0), 0.2);
        assert_eq!(clamp.eval(0.1), 0.2);
        assert_eq!(clamp.eval(0.9), 0.4);
        assert_eq!(clamp.eval(1.0), 0.4);
        let ext = c(Outside::Extend);
        // First segment slope 1.6, last -0.8; clamped to the 0–1 range.
        assert!((ext.eval(0.15) - (0.2 - 0.16)).abs() < 1e-12);
        assert_eq!(ext.eval(0.0), 0.0);
        assert!((ext.eval(1.0) - (0.4 - 0.2)).abs() < 1e-12);
        assert_eq!(
            Curve::new(vec![(0.2, 0.9), (0.4, 1.0)])
                .with_outside(Outside::Extend)
                .eval(1.0),
            1.0
        );
        let rep = c(Outside::Repeat);
        let refl = c(Outside::Reflect);
        // The span is 0.5; x = 0.75 itself is the last point, so the repeat
        // starts just past it.
        for k in 1..50 {
            let d = k as f64 / 100.0;
            let inside = 0.25 + d;
            for period in [-2.0, -1.0, 1.0, 2.0] {
                let x = inside + period * 0.5;
                assert!(
                    (rep.eval(x) - rep.eval(inside)).abs() < 1e-12,
                    "repeat at {x}"
                );
            }
            assert!(
                (refl.eval(0.25 - d) - refl.eval(0.25 + d)).abs() < 1e-12,
                "reflect left {d}"
            );
            assert!(
                (refl.eval(0.75 + d) - refl.eval(0.75 - d)).abs() < 1e-12,
                "reflect right {d}"
            );
            assert!(
                (refl.eval(0.75 + 0.5 + d) - refl.eval(0.25 + d)).abs() < 1e-12,
                "reflect twice {d}"
            );
        }
        assert_eq!(rep.eval(0.75), 0.4);
        assert_eq!(rep.eval(0.25), 0.2);
        // Repeat jumps back to the first value one span past the end; Reflect
        // is continuous there.
        assert!((rep.eval(0.75 + 1e-9) - 0.2).abs() < 1e-6);
        assert!((refl.eval(0.75 + 1e-9) - 0.4).abs() < 1e-6);
        // Inside the points every policy is the same curve.
        for x in xs().filter(|x| (0.25..=0.75).contains(x)) {
            for o in Outside::ALL {
                assert_eq!(
                    c(o).eval(x).to_bits(),
                    clamp.eval(x).to_bits(),
                    "{o:?} at {x}"
                );
            }
        }
        // Text keeps the policy; unknown policies are refused.
        for o in Outside::ALL {
            let t = c(o).to_text();
            assert_eq!(Curve::from_text(&t).unwrap().outside, o, "{t}");
        }
        assert!(Curve::from_text("0@wrap;0,0;1,1").is_none());
    }

    #[test]
    fn smoothness_changes_the_path_predictably_and_never_the_points() {
        // §107 / §252: the same points at 0, 50 and 100 %.
        let pts = vec![(0.0, 0.0), (0.2, 0.7), (0.6, 0.8), (1.0, 1.0)];
        let at = |s: f64| Curve::new(pts.clone()).with_smoothness(s);
        let (s0, s50, s100) = (at(0.0), at(0.5), at(1.0));
        let mut differs = 0;
        for c in [&s0, &s50, &s100] {
            let mut prev = -1.0;
            for x in xs() {
                let y = c.eval(x);
                assert!(y >= prev - 1e-12, "monotone data stays monotone at {x}");
                prev = y;
            }
            assert_eq!(c.points, pts);
            for &(x, y) in &pts {
                assert_eq!(c.eval(x), y);
            }
        }
        for x in xs().filter(|x| (0.0..=1.0).contains(x)) {
            let (a, m, b) = (s0.eval(x), s50.eval(x), s100.eval(x));
            // 0 % is the straight line, 50 % halfway to 100 %.
            assert!((m - (a + b) / 2.0).abs() < 1e-12, "x {x}: {a} {m} {b}");
            if (a - b).abs() > 1e-6 {
                differs += 1;
            }
        }
        assert!(differs > 100, "smoothness changed only {differs} samples");
        // The straight path is exactly linear between points.
        assert!((s0.eval(0.1) - 0.35).abs() < 1e-12);
        // Smoothness is written and read; v0.8's flags read as 0 / 100 %.
        let t = s50.to_text();
        assert!(t.starts_with("0.5;"), "{t}");
        assert_eq!(Curve::from_text(&t).unwrap(), s50);
        assert_eq!(
            Curve::from_text("1;0,0;0.5,0.2;1,1").unwrap().smoothness,
            1.0
        );
        assert_eq!(Curve::from_text("0;0,0;1,1").unwrap().smoothness, 0.0);
        assert!(Curve::from_text("1.5;0,0;1,1").is_none());
        assert!(Curve::from_text("-0.1;0,0;1,1").is_none());
        // NaN smoothness reads as straight; sanitize fixes it to 0.
        let nan = at(f64::NAN);
        assert_eq!(nan.eval(0.1), s0.eval(0.1));
        assert_eq!(nan.sanitized().smoothness, 0.0);
    }

    /// B1-0112 (§017): each point's own smoothness. Every point at the
    /// graph's smoothness gives the global path at 0 / 50 / 100 %; a point at
    /// 0 is a corner; mixed points stay monotone through their values; the
    /// points never move; the text keeps each point's value.
    #[test]
    fn per_point_smoothness_makes_corners_and_keeps_the_points() {
        let pts = vec![(0.0, 0.0), (0.2, 0.7), (0.6, 0.8), (1.0, 1.0)];
        for s in [0.0, 0.5, 1.0] {
            let global = Curve::new(pts.clone()).with_smoothness(s);
            let mut each = global.clone();
            each.local = vec![Some(s); 4];
            for x in xs() {
                assert!((global.eval(x) - each.eval(x)).abs() < 1e-12, "{s} at {x}");
            }
        }
        // Smooth graph, point 2 Not Smooth: a corner there (the slopes
        // either side are its secants), the other segments still curve.
        let c = Curve::new(pts.clone())
            .with_smoothness(1.0)
            .with_local(1, Some(0.0));
        let d = 1e-6;
        let left = (c.eval(0.2) - c.eval(0.2 - d)) / d;
        let right = (c.eval(0.2 + d) - c.eval(0.2)) / d;
        assert!((left - 0.7 / 0.2).abs() < 1e-3, "{left}");
        assert!((right - 0.1 / 0.4).abs() < 1e-3, "{right}");
        let smooth = Curve::new(pts.clone()).with_smoothness(1.0);
        // The segment not touching the corner is the smooth graph's, curved.
        assert!((c.eval(0.8) - smooth.eval(0.8)).abs() < 1e-12);
        assert!(
            (c.eval(0.8) - 0.9).abs() > 1e-4,
            "the far segment still curves"
        );
        // Straight graph, point 3 Smooth: only its two segments curve.
        let c2 = Curve::new(pts.clone()).with_local(2, Some(1.0));
        assert_eq!(
            c2.eval(0.1),
            0.35,
            "a segment between straight points stays straight"
        );
        assert!((c2.eval(0.4) - 0.75).abs() > 1e-4);
        for c in [&c, &c2] {
            let mut prev = -1.0;
            for x in xs() {
                let y = c.eval(x);
                assert!(y >= prev - 1e-12, "monotone at {x}");
                prev = y;
            }
            for &(x, y) in &pts {
                assert_eq!(c.eval(x), y, "the points never move");
            }
            let t = c.to_text();
            assert_eq!(Curve::from_text(&t).unwrap(), c.sanitized(), "{t}");
        }
        assert_eq!(
            c.to_text(),
            "1;0.000000,0.000000;0.200000,0.700000,0;0.600000,0.800000;1.000000,1.000000"
        );
        // The local values travel with their points through sorting.
        let mut shuffled = Curve::new(vec![(1.0, 1.0), (0.0, 0.0), (0.5, 0.2)]);
        shuffled.local = vec![Some(1.0), None, Some(0.25)];
        let s = shuffled.sanitized();
        assert_eq!(s.points, vec![(0.0, 0.0), (0.5, 0.2), (1.0, 1.0)]);
        assert_eq!(s.local, vec![None, Some(0.25), Some(1.0)]);
        // All None normalizes to empty (equal to a v0.9 curve).
        let mut none = Curve::identity();
        none.local = vec![None, None];
        assert_eq!(none.sanitized(), Curve::identity());
        // Invalid local values are refused in text and fixed in values.
        assert!(Curve::from_text("0;0,0,2;1,1").is_none());
        assert!(Curve::from_text("0;0,0,x;1,1").is_none());
        assert!(Curve::from_text("0;0,0,0,0;1,1").is_none());
        let mut nan = Curve::identity();
        nan.local = vec![Some(f64::NAN), Some(7.0)];
        assert_eq!(nan.sanitized().local, vec![Some(0.0), Some(1.0)]);
    }

    /// B1-0113: overshoot widens the values the graph may take; text round
    /// trips; v0.9 texts are unchanged; parameters that do not allow it read
    /// the curve pinned into 0 – 1.
    #[test]
    fn overshoot_widens_the_range_only_where_the_parameter_allows_it() {
        let c = Curve::new(vec![(0.0, -0.4), (0.5, 1.3), (1.0, 0.2)])
            .with_smoothness(0.5)
            .with_overshoot(0.5);
        assert_eq!(c.y_range(), (-0.5, 1.5));
        let c = c.sanitized();
        assert_eq!(c.points[0].1, -0.4);
        assert_eq!(c.eval(0.5), 1.3);
        assert_eq!(c.eval(0.0), -0.4);
        for x in xs() {
            let y = c.eval(x);
            assert!((-0.5..=1.5).contains(&y), "{x}: {y}");
        }
        // Extend continues past 1 within the overshoot range.
        let e = Curve::new(vec![(0.2, 0.8), (0.4, 1.2)])
            .with_overshoot(0.5)
            .with_outside(Outside::Extend);
        assert!((e.eval(0.5) - 1.4).abs() < 1e-12);
        assert_eq!(e.eval(1.0), 1.5, "clamped at the effective range");
        // Extreme: the largest overshoot; beyond it is pinned.
        let x = Curve::new(vec![(0.0, -9.0), (1.0, 9.0)])
            .with_overshoot(9.0)
            .sanitized();
        assert_eq!(x.overshoot, MAX_OVERSHOOT);
        assert_eq!(x.points, vec![(0.0, -2.0), (1.0, 3.0)]);
        // Text: `^o`, round trip; v0.9 texts read and print unchanged.
        let t = c.to_text();
        assert!(t.starts_with("0.5^0.5;"), "{t}");
        assert_eq!(Curve::from_text(&t).unwrap(), c);
        let t2 = e.sanitized().to_text();
        assert!(t2.starts_with("0@extend^0.5;"), "{t2}");
        assert_eq!(Curve::from_text(&t2).unwrap(), e.sanitized());
        for v09 in [
            "0;0.000000,0.000000;1.000000,1.000000",
            "0.5@reflect;0.100000,0.200000;0.900000,0.700000",
            "1;0.000000,1.000000;0.500000,0.200000;1.000000,0.900000",
        ] {
            let c = Curve::from_text(v09).unwrap();
            assert!(c.local.is_empty() && c.overshoot == 0.0);
            assert_eq!(c.to_text(), v09);
        }
        assert!(Curve::from_text("0^3;0,0;1,1").is_none());
        assert!(Curve::from_text("0^-1;0,0;1,1").is_none());
        assert!(Curve::from_text("0^x;0,0;1,1").is_none());
        // Without overshoot a y beyond 0 – 1 is pinned (as in v0.9).
        assert_eq!(
            Curve::from_text("0;0,-0.5;1,1.5").unwrap().points,
            vec![(0.0, 0.0), (1.0, 1.0)]
        );
        // Params: only a parameter declared `.overshoot()` sees it.
        let specs = vec![
            curve("plain", "Plain", Curve::identity()),
            curve("wide", "Wide", Curve::identity()).overshoot(),
        ];
        let mut set = ParamSet::new();
        set.set("plain", ParamValue::Curve(c.clone()));
        set.set("wide", ParamValue::Curve(c.clone()));
        let p = Params::new(&specs, &set);
        let plain = p.curve("plain");
        assert_eq!(plain.overshoot, 0.0);
        assert_eq!(plain.points[0].1, 0.0);
        assert_eq!(plain.points[1].1, 1.0);
        for x in xs() {
            assert!((0.0..=1.0).contains(&plain.eval(x)));
        }
        assert_eq!(p.curve("wide"), c);
    }
}
