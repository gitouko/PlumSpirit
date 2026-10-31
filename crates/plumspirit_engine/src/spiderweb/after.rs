//! Changes made to a shape's notes after they are made: the claw machine and
//! strum (ports of Spiderweb `claw.py` and `strum.py`, MIT © Kanade
//! Tachibana). Rows are `(start, end, key, velocity, track)`; the track column
//! rides along.

use super::curves::rne;
use super::rng::NpRng;
use super::{fnum, inum};
use crate::json::Value;

pub type Row = [i64; 5];

// ================================================================ claw

const PERIODS: [f64; 5] = [1.0, 2.0, 4.0, 8.0, 16.0];
const TRASHES: [(f64, f64); 9] = [
    (1.0, 4.0),
    (2.0, 4.0),
    (3.0, 4.0),
    (4.0, 4.0),
    (1.0, 3.0),
    (2.0, 3.0),
    (3.0, 3.0),
    (1.0, 2.0),
    (2.0, 2.0),
];
const SHORT: f64 = 1.0 / 24.0;
const MAX_COUNT: f64 = 1000.0;

/// Python `min(a, b)`: `b` only when it is smaller.
#[inline]
pub fn pmin(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// Python `max(a, b)`.
#[inline]
pub fn pmax(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

fn bend_k() -> f64 {
    50.0f64.ln()
}

/// A claw from a file → valid claw, or None (none / changes nothing).
pub fn clean_claw(c: Option<&Value>) -> Option<Value> {
    let c = c?;
    if !matches!(c, Value::Obj(_)) {
        return None;
    }
    let mode = c
        .get("mode")
        .and_then(Value::as_str)
        .filter(|m| matches!(*m, "time" | "notes" | "keys" | "chords" | "random"))
        .unwrap_or("time");
    // Python `max(lo, min(hi, v))` (a NaN goes to `hi`).
    let clamp = |v: f64, lo: f64, hi: f64| pmax(lo, pmin(hi, v));
    let read = |key: &str, cur: f64, lo: f64, hi: f64, int: bool| -> f64 {
        match c.get(key).map_or(Some(cur), fnum) {
            Some(v) if int && !v.is_finite() => cur,
            Some(v) => clamp(if int { v.trunc() } else { v }, lo, hi),
            None => cur,
        }
    };
    let keep = read("keep", 1.0, 1.0, MAX_COUNT, true);
    let skip = read("skip", 1.0, 0.0, MAX_COUNT, true);
    let pct = read("pct", 50.0, 0.0, 100.0, false);
    let seed = read("seed", 1.0, 0.0, 1e9, true);
    let cut = read("cut", 25.0, 1.0, 99.0, false);
    let period = c
        .get("period")
        .and_then(fnum)
        .filter(|p| PERIODS.contains(p))
        .unwrap_or(1.0);
    let trash = c.get("trash").and_then(Value::as_arr).and_then(|t| {
        if t.len() != 2 {
            return None;
        }
        let pair = (fnum(&t[0])?, fnum(&t[1])?);
        TRASHES.contains(&pair).then_some(pair)
    });
    let dist = c
        .get("dist")
        .map_or(Some(0.0), fnum)
        .map_or(0.0, |d| clamp(d, -100.0, 100.0));
    let flag = |k: &str| c.get(k) == Some(&Value::Bool(true));
    let (stretch, short, shorten) = (flag("stretch"), flag("short"), flag("shorten"));
    let counts = matches!(mode, "notes" | "keys" | "chords");
    if (mode == "time" && trash.is_none() && dist == 0.0)
        || (counts && skip == 0.0)
        || (mode == "random" && pct == 100.0)
    {
        return None;
    }
    let mut out = Value::obj();
    out.set("mode", Value::Str(mode.to_owned()));
    let num = Value::Num;
    match mode {
        "time" => {
            out.set("period", num(period));
            out.set(
                "trash",
                match trash {
                    Some((a, b)) => Value::Arr(vec![num(a), num(b)]),
                    None => Value::Null,
                },
            );
            out.set("dist", num(dist));
            out.set("stretch", Value::Bool(stretch));
            out.set("short", Value::Bool(short));
        }
        "random" => {
            out.set("pct", num(pct));
            out.set("seed", num(seed));
            out.set("shorten", Value::Bool(shorten));
            out.set("cut", num(cut));
        }
        _ => {
            out.set("keep", num(keep));
            out.set("skip", num(skip));
            out.set("shorten", Value::Bool(shorten));
            out.set("cut", num(cut));
        }
    }
    Some(out)
}

fn bend(y: f64, period: f64, dist: f64) -> f64 {
    let a = bend_k() * dist / 100.0;
    let w = (y / period).floor();
    let x = y / period - w;
    (w + (1.0 - (-a * x).exp()) / (1.0 - (-a).exp())) * period
}

/// The notes after the claw.
pub fn apply_claw(a: Vec<Row>, claw: &Value, ppq: u32) -> Vec<Row> {
    if a.is_empty() {
        return a;
    }
    let g = |k: &str| claw.get(k).and_then(fnum).unwrap_or(0.0);
    let mode = claw.get("mode").and_then(Value::as_str).unwrap_or("time");
    if mode != "time" {
        let keep = picked(&a, claw, mode);
        if claw.get("shorten") != Some(&Value::Bool(true)) {
            return a
                .into_iter()
                .zip(keep)
                .filter(|(_, k)| *k)
                .map(|(r, _)| r)
                .collect();
        }
        let cut = g("cut");
        return a
            .into_iter()
            .zip(keep)
            .map(|(mut r, k)| {
                if !k {
                    let len = rne((r[1] - r[0]) as f64 * cut / 100.0).max(1.0);
                    r[1] = r[0] + len as i64;
                }
                r
            })
            .collect();
    }
    let s: Vec<f64> = a.iter().map(|r| r[0] as f64).collect();
    let e: Vec<f64> = a.iter().map(|r| r[1] as f64).collect();
    let t0 = s.iter().copied().fold(f64::INFINITY, f64::min);
    let period = g("period") * f64::from(ppq);
    let mut xs: Vec<f64> = s.iter().map(|v| v - t0).collect();
    let mut xe: Vec<f64> = e.iter().map(|v| v - t0).collect();
    if let Some(t) = claw.get("trash").and_then(Value::as_arr)
        && t.len() == 2
    {
        let (k, n) = (fnum(&t[0]).unwrap_or(1.0), fnum(&t[1]).unwrap_or(1.0));
        let cut = period / n;
        let lo = (k - 1.0) * cut;
        let close =
            |x: f64| x - cut * (x / period).floor() - (x.rem_euclid(period) - lo).clamp(0.0, cut);
        for v in xs.iter_mut().chain(xe.iter_mut()) {
            *v = close(*v);
        }
        if claw.get("stretch") == Some(&Value::Bool(true)) {
            let full = e.iter().map(|v| v - t0).fold(f64::NEG_INFINITY, f64::max);
            let left = close(full);
            if left > 0.0 {
                let f = full / left;
                for v in xs.iter_mut().chain(xe.iter_mut()) {
                    *v *= f;
                }
            }
        }
    }
    let mut keep: Vec<bool> = xs.iter().zip(&xe).map(|(a, b)| b - a > 1e-6).collect();
    let dist = g("dist");
    if dist != 0.0 {
        for v in xs.iter_mut().chain(xe.iter_mut()) {
            *v = bend(*v, period, dist);
        }
    }
    if claw.get("short") == Some(&Value::Bool(true)) {
        let short = SHORT * f64::from(ppq);
        for i in 0..a.len() {
            keep[i] &= (xe[i] - xs[i] >= short - 1e-6) || (e[i] - s[i] < short);
        }
    }
    let mut out = Vec::with_capacity(a.len());
    for (i, mut r) in a.into_iter().enumerate() {
        if !keep[i] {
            continue;
        }
        let st = rne(xs[i] + t0) as i64;
        r[0] = st;
        r[1] = (rne(xe[i] + t0) as i64).max(st + 1);
        out.push(r);
    }
    out
}

/// Which notes a counting / random claw keeps.
fn picked(a: &[Row], claw: &Value, mode: &str) -> Vec<bool> {
    let n = a.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (a[i][0], a[i][2]));
    if mode == "random" {
        let seed = claw.get("seed").and_then(inum).unwrap_or(1).max(0) as u64;
        let pct = claw.get("pct").and_then(fnum).unwrap_or(50.0);
        let mut rng = NpRng::new(seed);
        let mut keep = vec![false; n];
        for &i in &order {
            keep[i] = rng.random() < pct / 100.0;
        }
        return keep;
    }
    let num: Vec<i64> = match mode {
        "notes" => {
            let mut v = vec![0i64; n];
            for (rank, &i) in order.iter().enumerate() {
                v[i] = rank as i64;
            }
            v
        }
        "keys" => {
            let lo = a.iter().map(|r| r[2]).min().unwrap_or(0);
            a.iter().map(|r| r[2] - lo).collect()
        }
        _ => {
            let mut starts: Vec<i64> = a.iter().map(|r| r[0]).collect();
            starts.sort_unstable();
            starts.dedup();
            a.iter()
                .map(|r| starts.binary_search(&r[0]).expect("present") as i64)
                .collect()
        }
    };
    let keep = claw.get("keep").and_then(inum).unwrap_or(1);
    let skip = claw.get("skip").and_then(inum).unwrap_or(1);
    num.iter()
        .map(|v| v.rem_euclid(keep + skip) < keep)
        .collect()
}

// ================================================================ strum

const STRUM_LIMITS: [(&str, f64, f64); 6] = [
    ("time", -4.0, 4.0),
    ("tension", -100.0, 100.0),
    ("vel", -127.0, 127.0),
    ("vel_tension", -100.0, 100.0),
    ("end_time", -4.0, 4.0),
    ("end_tension", -100.0, 100.0),
];
const STRUM_FLAGS: [(&str, bool); 6] = [
    ("start", true),
    ("preserve", false),
    ("ahead", false),
    ("end", true),
    ("chop", false),
    ("alternate", false),
];
const CAP: f64 = 1e8;
const CHOP_MAX: usize = 5_000_000;

/// A strum from a file → valid strum, or None (none / changes nothing).
pub fn clean_strum(c: Option<&Value>) -> Option<Value> {
    let c = c?;
    if !matches!(c, Value::Obj(_)) {
        return None;
    }
    let mut out = Value::obj();
    for (k, lo, hi) in STRUM_LIMITS {
        let v = match c.get(k).map_or(Some(0.0), fnum) {
            Some(v) if v.is_finite() => v.clamp(lo, hi),
            _ => 0.0,
        };
        out.set(k, Value::Num(v));
    }
    for (k, d) in STRUM_FLAGS {
        let v = c.get(k).and_then(Value::as_bool).unwrap_or(d);
        out.set(k, Value::Bool(v));
    }
    let f = |k: &str| out.get(k).and_then(fnum).unwrap_or(0.0);
    let b = |k: &str| out.get(k) == Some(&Value::Bool(true));
    if !(b("start") && (f("time") != 0.0 || f("vel") != 0.0) || b("end") && f("end_time") != 0.0) {
        return None;
    }
    Some(out)
}

/// `g + g·r + …` (k terms), capped.
pub fn steps(k: f64, g: f64, tension: f64) -> f64 {
    let r = if tension >= 0.0 {
        1.0 + tension / 100.0
    } else {
        1.0 + tension / 200.0
    };
    if r == 1.0 {
        return (g * k).min(CAP);
    }
    let v = g * (r.powf(k) - 1.0) / (r - 1.0);
    if v.is_nan() { CAP } else { v.min(CAP) }
}

/// Every note cut where another note starts while it is held.
pub fn chop(a: Vec<Row>) -> Vec<Row> {
    let mut cuts: Vec<i64> = a.iter().map(|r| r[0]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut total = 0usize;
    let info: Vec<(usize, usize)> = a
        .iter()
        .map(|r| {
            let lo = cuts.partition_point(|&c| c <= r[0]);
            let hi = cuts.partition_point(|&c| c < r[1]);
            let n = hi.saturating_sub(lo);
            total += n;
            (lo, n)
        })
        .collect();
    if total == 0 || total > CHOP_MAX {
        return a;
    }
    let mut out = Vec::with_capacity(a.len() + total);
    for (r, (lo, n)) in a.iter().zip(info) {
        for j in 0..=n {
            let mut p = *r;
            p[0] = if j == 0 { r[0] } else { cuts[lo + j - 1] };
            p[1] = if j == n { r[1] } else { cuts[lo + j] };
            out.push(p);
        }
    }
    out
}

/// The notes strummed (sorted by start, then key).
pub fn apply_strum(a: Vec<Row>, st: &Value, ppq: u32) -> Vec<Row> {
    if a.is_empty() {
        return a;
    }
    let f = |k: &str| st.get(k).and_then(fnum).unwrap_or(0.0);
    let b = |k: &str| st.get(k) == Some(&Value::Bool(true));
    let mut a = if b("chop") { chop(a) } else { a };
    a.sort_by_key(|r| (r[0], r[2]));
    let n = a.len();
    let mut firsts = Vec::new();
    let mut chord = vec![0usize; n];
    for i in 0..n {
        if i == 0 || a[i][0] != a[i - 1][0] {
            firsts.push(i);
        }
        chord[i] = firsts.len() - 1;
    }
    let size: Vec<usize> = (0..firsts.len())
        .map(|c| firsts.get(c + 1).copied().unwrap_or(n) - firsts[c])
        .collect();
    let start = b("start");
    let mut down = vec![start && f("time") < 0.0; firsts.len()];
    if b("alternate") {
        let mut count = 0;
        for (c, d) in down.iter_mut().enumerate() {
            if size[c] > 1 {
                count += 1;
                if count % 2 == 0 {
                    *d = !*d;
                }
            }
        }
    }
    let ppqf = f64::from(ppq);
    for i in 0..n {
        let c = chord[i];
        let m = size[c] as f64;
        let rank = (i - firsts[c]) as f64;
        let q = if down[c] { m - 1.0 - rank } else { rank };
        let mut ns = a[i][0] as f64;
        let mut ne = a[i][1] as f64;
        if start && f("time") != 0.0 {
            let g = f("time").abs() * ppqf;
            let off = if b("ahead") {
                steps(q, g, -f("tension")) - steps(m - 1.0, g, -f("tension"))
            } else {
                steps(q, g, f("tension"))
            };
            let off = rne(off);
            ns += off;
            if !b("preserve") {
                ne += off;
            }
        }
        if b("end") && f("end_time") != 0.0 {
            let k = if f("end_time") > 0.0 { m - 1.0 - q } else { q };
            ne -= rne(steps(k, f("end_time").abs() * ppqf, f("end_tension")));
        }
        if start && f("vel") != 0.0 {
            let k = if f("vel") > 0.0 { q } else { m - 1.0 - q };
            let v = (a[i][3] as f64 - rne(steps(k, f("vel").abs(), f("vel_tension")))).max(1.0);
            a[i][3] = v as i64;
        }
        let ns = ns.max(0.0);
        a[i][0] = ns as i64;
        a[i][1] = ne.max(ns + 1.0) as i64;
    }
    a
}

// ================================================================ Colours (cycle)

const CYCLE_MAX: i64 = 15;

/// A saved `cycle` → a valid one, or None (off).
pub fn clean_cycle(c: Option<&Value>) -> Option<Value> {
    let c = c?;
    let by = c.get("by").and_then(Value::as_str)?;
    if !matches!(by, "step" | "key" | "time") {
        return None;
    }
    let n = inum(c.get("n").unwrap_or(&Value::Num(2.0)))?.clamp(2, CYCLE_MAX);
    let every = c.get("every").cloned().unwrap_or(Value::Num(1.0));
    let every = if by == "time" {
        let (a, b) = match every.as_arr() {
            Some(t) if t.len() == 2 => (inum(&t[0])?, inum(&t[1])?),
            _ => (1, 4),
        };
        Value::Arr(vec![
            Value::Num(a.clamp(1, 10_000) as f64),
            Value::Num(b.clamp(1, 10_000) as f64),
        ])
    } else {
        Value::Num(inum(&every)?.clamp(1, 10_000) as f64)
    };
    let mut out = Value::obj();
    out.set("by", Value::Str(by.to_owned()));
    out.set("n", Value::Num(n as f64));
    out.set("every", every);
    Some(out)
}

/// Which turn (0 .. n − 1) each note gets ("Colours").
pub fn cycle_turns(sh: &Value, notes: &[[i64; 3]], ppq: u32) -> Vec<i64> {
    let c = sh.get("cycle").expect("cycling");
    let n = c.get("n").and_then(inum).unwrap_or(2).max(1);
    let by = c.get("by").and_then(Value::as_str).unwrap_or("step");
    let s: Vec<i64> = notes.iter().map(|r| r[0]).collect();
    let k: Vec<i64> = match by {
        "key" => {
            let lo = notes.iter().map(|r| r[2]).min().unwrap_or(0);
            notes.iter().map(|r| r[2] - lo).collect()
        }
        "time" => {
            let ev = c.get("every").and_then(Value::as_arr).unwrap_or(&[]);
            let a = ev.first().and_then(inum).unwrap_or(1) as f64;
            let b = ev.get(1).and_then(inum).unwrap_or(4);
            let d = 4.0 * a * f64::from(ppq);
            return s
                .iter()
                .map(|&x| (((x * b) as f64 / d + 1e-9).floor() as i64).rem_euclid(n))
                .collect();
        }
        _ if super::kind(sh) == "custom"
            && matches!(super::gets(sh, "fill"), Some("spam" | "outline_spam")) =>
        {
            let g = (super::getf(sh, "gate", 0.0625) * f64::from(ppq)).max(1.0);
            let aligned = super::gets(sh, "align") == Some("aligned")
                && !sh.get("hz").is_some_and(Value::truthy);
            let x0 = if aligned {
                0
            } else {
                *s.iter().min().unwrap_or(&0)
            };
            s.iter()
                .map(|&x| ((x - x0) as f64 / g + 0.5).floor() as i64)
                .collect()
        }
        _ => {
            let mut u = s.clone();
            u.sort_unstable();
            u.dedup();
            s.iter()
                .map(|x| u.binary_search(x).expect("present") as i64)
                .collect()
        }
    };
    let every = c.get("every").and_then(inum).unwrap_or(1).max(1);
    k.iter()
        .map(|v| v.div_euclid(every).rem_euclid(n))
        .collect()
}
