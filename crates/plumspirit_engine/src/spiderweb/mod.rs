//! Spiderweb (v0.8 built-in plugin): a port of the note engine of
//! Spiderweb by Kanade Tachibana (MIT licence, pinned at f7fd766;
//! `docs/references/v0_8/SPIDERWEB_NOTES_SPEC.md`), expanded for PlumSpirit by
//! ouko. Shapes drawn in `(beat, pitch)` become notes exactly as Spiderweb
//! makes them; the app pastes them through `transform::paste`.
//!
//! Shapes are kept as cleaned JSON objects (`json::Value`), the same schema
//! Spiderweb saves and shares, so share codes and project shapes read back
//! unchanged and every rule ports one to one.

pub mod after;
pub mod areas;
pub mod curves;
pub mod custom;
pub mod edit;
pub mod engine;
pub mod fit;
pub mod formula;
pub mod funnel;
pub mod hzbass;
pub mod joined;
pub mod np;
pub mod output;
pub mod paths;
pub mod pattern;
pub mod polygon;
pub mod pyint;
pub mod rng;
pub mod share;
pub mod shrink;
pub mod smooth;
pub mod text;
pub mod tumour;

use crate::json::Value;
use curves::rne;
use engine::{Note4, unique_rows};
use paths::{P, Raw, dedupe, dot_segment_notes, path_notes};

pub const KINDS: [&str; 7] = ["line", "poly", "free", "curve", "arc", "custom", "funnel"];
/// Kinds that can have tumours.
pub const LINE_KINDS: [&str; 5] = ["line", "poly", "free", "curve", "arc"];
/// Kinds that can have shape / pattern formulas.
pub const FORMULA_KINDS: [&str; 4] = ["curve", "line", "poly", "arc"];

/// A number in a JSON value (Python `float(x)`: numbers, bools, numeric text).
pub fn fnum(v: &Value) -> Option<f64> {
    match v {
        Value::Num(n) => Some(*n),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Str(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Python `int(x)` (truncation; numeric text).
pub fn inum(v: &Value) -> Option<i64> {
    match v {
        Value::Num(n) if n.is_finite() => Some(n.trunc() as i64),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Str(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// `sh.get(key, default)` as a float.
pub fn getf(sh: &Value, key: &str, default: f64) -> f64 {
    sh.get(key).and_then(fnum).unwrap_or(default)
}

pub fn gets<'a>(sh: &'a Value, key: &str) -> Option<&'a str> {
    sh.get(key).and_then(Value::as_str)
}

pub fn kind(sh: &Value) -> &str {
    gets(sh, "kind").unwrap_or("")
}

/// `[[x, y], ...]` → points (None when malformed).
pub fn pts_of(v: &Value) -> Option<Vec<P>> {
    let a = v.as_arr()?;
    let mut out = Vec::with_capacity(a.len());
    for p in a {
        let q = p.as_arr()?;
        if q.len() != 2 {
            return None;
        }
        out.push([fnum(&q[0])?, fnum(&q[1])?]);
    }
    Some(out)
}

pub fn pts_value(pts: &[P]) -> Value {
    Value::Arr(
        pts.iter()
            .map(|p| Value::Arr(vec![Value::Num(p[0]), Value::Num(p[1])]))
            .collect(),
    )
}

/// The shape's points (cleaned shapes always have them).
pub fn shape_pts(sh: &Value) -> Vec<P> {
    sh.get("pts").and_then(pts_of).unwrap_or_default()
}

/// Shape loaded from a file → valid shape, or None (unsupported / broken).
pub fn clean_shape(sh: &Value) -> Option<Value> {
    let k = kind(sh);
    if !KINDS.contains(&k) {
        return None;
    }
    let pts = pts_of(sh.get("pts")?)?;
    if pts.is_empty() {
        return None;
    }
    let mut out = Value::obj();
    out.set("vel0", sh.get("vel0").cloned().unwrap_or(Value::Num(127.0)));
    out.set("vel1", sh.get("vel1").cloned().unwrap_or(Value::Num(127.0)));
    out.set(
        "end_dot",
        Value::Bool(sh.get("end_dot").is_some_and(Value::truthy)),
    );
    out.set("kind", Value::Str(k.to_owned()));
    out.set("pts", pts_value(&pts));
    if let Some(env) = sh.get("vel_env").filter(|e| e.truthy()) {
        let mut e = Vec::new();
        for p in env.as_arr()? {
            let q = p.as_arr()?;
            if q.len() != 2 {
                return None;
            }
            e.push(Value::Arr(vec![
                Value::Num(fnum(&q[0])?),
                Value::Num(fnum(&q[1])?.clamp(1.0, 127.0)),
            ]));
        }
        out.set("vel_env", Value::Arr(e));
    }
    if let Some(c) = after::clean_claw(sh.get("claw")) {
        out.set("claw", c);
    }
    if let Some(c) = after::clean_strum(sh.get("strum")) {
        out.set("strum", c);
    }
    if let Some(c) = after::clean_cycle(sh.get("cycle")) {
        out.set("cycle", c);
    }
    if LINE_KINDS.contains(&k)
        && let Some(t) = tumour::clean_tumour(sh.get("tumour"))
    {
        out.set("tumour", t);
    }
    if k == "custom" {
        custom::clean_custom(sh, &mut out)?;
    }
    if k == "funnel" {
        funnel::clean_shape_funnel(sh, &mut out)?;
    }
    if k == "arc" {
        if pts.len() != 3 {
            return None;
        }
        out.set("k", Value::Num(curves::arc_k(sh.get("k").and_then(fnum))));
    }
    if k == "free" && sh.has("smooth") {
        out.set(
            "smooth",
            Value::Num(smooth::clean_level(sh.get("smooth").and_then(fnum))),
        );
        out.set("k", Value::Num(curves::arc_k(sh.get("k").and_then(fnum))));
    }
    if k == "curve" {
        let n = pts.len();
        if n < 4 {
            return None;
        }
        let keep = n - (n - 1) % 3;
        out.set("pts", pts_value(&pts[..keep]));
        let last = (keep as i64 - 1) / 3;
        // all or nothing: one bad entry and Python keeps no corners
        let sharp: Vec<i64> = match sh.get("sharp") {
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
        if let Some(sym) = gets(sh, "sym")
            && matches!(sym, "mirror" | "turn")
            && last % 2 == 0
        {
            out.set("sym", Value::Str(sym.to_owned()));
        }
        set_formulas(sh, &mut out);
        joined::clean_joined(sh, &mut out);
        if joined::is_joined(&out)
            && let Value::Obj(o) = &mut out
        {
            o.retain(|(k, _)| k != "sym");
        }
    }
    if FORMULA_KINDS.contains(&k) {
        set_formulas(sh, &mut out);
    }
    // PlumSpirit's own settings (Spiderweb ignores them)
    if let Some(ps) = edit::clean_ps(sh.get(edit::PS)) {
        out.set(edit::PS, ps);
    }
    Some(out)
}

/// A shape's `pattern` / `shape` formulas, cleaned (pattern.py).
fn set_formulas(sh: &Value, out: &mut Value) {
    if let Some(p) = pattern::clean_pattern(sh.get("pattern")) {
        out.set("pattern", p);
    }
    if let Some(f) = pattern::clean_shape_formula(sh.get("shape")) {
        out.set("shape", f);
    }
}

/// The shape as one polyline (custom shapes: all strokes one after another).
pub fn shape_path(sh: &Value) -> Vec<P> {
    if matches!(kind(sh), "custom" | "funnel") {
        return shape_strokes(sh).concat();
    }
    let path = base_path(sh);
    match sh.get("tumour") {
        Some(tm) if tm.get("on") == Some(&Value::Bool(true)) => tumour::tumour_path(&path, tm),
        _ => path,
    }
}

/// The shape's path before its tumours (with its formulas).
fn base_path(sh: &Value) -> Vec<P> {
    let pts = shape_pts(sh);
    let pts = match kind(sh) {
        "curve" => curves::sample(&pts, 240),
        "arc" => curves::arc_points(&pts, getf(sh, "k", 1.0)),
        "free" if getf(sh, "smooth", 0.0) != 0.0 => {
            smooth::smooth_path(&pts, getf(sh, "smooth", 0.0), getf(sh, "k", 1.0))
        }
        _ => pts,
    };
    if pattern::has_formula(sh) {
        return pattern::formed_paths(vec![pts], sh).swap_remove(0);
    }
    pts
}

/// The shape as separate polylines (custom shapes: their strokes; a joined
/// curve: one per piece).
pub fn shape_strokes(sh: &Value) -> Vec<Vec<P>> {
    if kind(sh) == "custom" {
        return custom::Custom::new(sh, 960).paths().clone();
    }
    if kind(sh) == "funnel" {
        return funnel::Funnel::new(sh).strokes();
    }
    if joined::is_joined(sh) {
        return joined::joined_paths(sh);
    }
    vec![shape_path(sh)]
}

/// The shape's velocity envelope (`vel_env`, or the vel0 → vel1 ramp).
pub fn velocity_env(sh: &Value) -> Vec<[f64; 2]> {
    if let Some(e) = sh.get("vel_env").filter(|e| e.truthy())
        && let Some(p) = pts_of(e)
    {
        return p;
    }
    vec![
        [0.0, getf(sh, "vel0", 127.0)],
        [1.0, getf(sh, "vel1", 127.0)],
    ]
}

/// A shape's notes `(start, end, pitch, velocity)` and, for shapes made of
/// tracks / groups, the track of each note — before claw and strum.
fn raw_notes_tracks(sh: &Value, ppq: u32, keys: i64) -> (Vec<Note4>, Option<Vec<i64>>) {
    let end_dot = sh.get("end_dot").is_some_and(Value::truthy);
    let k = kind(sh);
    let cs = (k == "custom").then(|| custom::Custom::new(sh, ppq));
    let strokes = match &cs {
        Some(c) => c.paths().clone(),
        None => shape_strokes(sh),
    };
    let all: Vec<P> = strokes.iter().flatten().copied().collect();
    let mut path = dedupe(&all);
    if path.is_empty() {
        return (Vec::new(), None);
    }
    if path[path.len() - 1][0] < path[0][0] {
        path.reverse();
    }
    let ppqf = f64::from(ppq);
    let path: Vec<P> = path.iter().map(|p| [p[0] * ppqf, p[1]]).collect();
    // pasted notes' own (velocity, track); a custom shape's groups
    let mut own: Option<Vec<[i64; 2]>> = None;
    let mut groups: Option<Vec<i64>> = None;
    let raw: Vec<Raw> = if let Some(c) = &cs {
        if sh.has("notes") {
            let b = custom::block_notes(sh, ppq);
            own = Some(b.iter().map(|r| [r[3], r[4]]).collect());
            b.iter().map(|r| [r[0], r[1], r[2]]).collect()
        } else {
            let (n, g) = c.notes_groups();
            groups = g;
            n
        }
    } else if k == "funnel" {
        funnel::funnel_notes(sh, ppq)
    } else if LINE_KINDS.contains(&k) && strokes.len() > 1 {
        let mut v = Vec::new();
        for a in &strokes {
            let mut a = dedupe(a);
            if !a.is_empty() && a[a.len() - 1][0] < a[0][0] {
                a.reverse();
            }
            let a: Vec<P> = a.iter().map(|p| [p[0] * ppqf, p[1]]).collect();
            v.extend(path_notes(&a, end_dot));
        }
        v
    } else if end_dot
        && k == "poly"
        && path.len() > 2
        && !(sh.get("shape").is_some_and(Value::truthy)
            || sh.get("pattern").is_some_and(Value::truthy))
    {
        dot_segment_notes(&path)
    } else {
        path_notes(&path, end_dot)
    };
    let t_lo = path.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let t_hi = path.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
    let env = velocity_env(sh);
    let keep: Vec<bool> = raw
        .iter()
        .map(|r| r[2] >= 0 && r[2] < keys && r[1] > 0)
        .collect();
    let pick = |v: Vec<Raw>| -> Vec<Raw> {
        v.into_iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(mut r, _)| {
                r[0] = r[0].max(0);
                r
            })
            .collect()
    };
    let raw = pick(raw);
    let mut tracks: Option<Vec<i64>> = None;
    let raw: Vec<Raw> = if let Some(own) = own {
        let own: Vec<[i64; 2]> = own
            .into_iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(o, _)| o)
            .collect();
        if sh.get("own_vel").is_some_and(Value::truthy) {
            let rows: Vec<[i64; 5]> = raw
                .iter()
                .zip(&own)
                .map(|(r, o)| [r[0], r[1], r[2], o[0], o[1]])
                .collect();
            let got = unique_rows(rows);
            return (
                got.iter().map(|r| [r[0], r[1], r[2], r[3]]).collect(),
                Some(got.iter().map(|r| r[4]).collect()),
            );
        }
        let rows: Vec<[i64; 4]> = raw
            .iter()
            .zip(&own)
            .map(|(r, o)| [r[0], r[1], r[2], o[1]])
            .collect();
        let got = unique_rows(rows);
        tracks = Some(got.iter().map(|r| r[3]).collect());
        got.iter().map(|r| [r[0], r[1], r[2]]).collect()
    } else if let Some(g) = groups {
        let g: Vec<i64> = g
            .into_iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(g, _)| g)
            .collect();
        let rows: Vec<[i64; 4]> = raw
            .iter()
            .zip(&g)
            .map(|(r, g)| [r[0], r[1], r[2], *g])
            .collect();
        let got = unique_rows(rows);
        tracks = Some(got.iter().map(|r| r[3]).collect());
        got.iter().map(|r| [r[0], r[1], r[2]]).collect()
    } else {
        let raw = unique_rows(raw);
        if cycling(sh) && !raw.is_empty() {
            tracks = Some(after::cycle_turns(sh, &raw, ppq));
        }
        raw
    };
    let constant = env.iter().all(|p| p[1] == env[0][1]);
    let vel: Vec<i64> = if constant {
        let v = (rne(env[0][1]) as i64).clamp(1, 127);
        vec![v; raw.len()]
    } else {
        raw.iter()
            .map(|r| {
                let frac = if t_hi > t_lo {
                    ((r[0] as f64 - t_lo) / (t_hi - t_lo)).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                (rne(curves::env_value(&env, frac)).clamp(1.0, 127.0)) as i64
            })
            .collect()
    };
    // Hz bass velocity effects
    let mut vel = vel;
    if k == "custom"
        && let Some(hz) = cs.as_ref().and_then(custom::Custom::hz)
        && !hz.tones.is_empty()
    {
        let starts: Vec<i64> = raw.iter().map(|r| r[0]).collect();
        let keys: Vec<i64> = raw.iter().map(|r| r[2]).collect();
        if let Some(f) = hzbass::velocity_factor(&hz, &shape_pts(sh), ppq, &starts, &keys) {
            for (v, f) in vel.iter_mut().zip(f) {
                *v = ((*v as f64 * f + 0.5).floor()).clamp(1.0, 127.0) as i64;
            }
        }
    }
    let notes = raw
        .iter()
        .zip(&vel)
        .map(|(r, v)| [r[0], r[1], r[2], *v])
        .collect();
    (notes, tracks)
}

/// The shape's notes after its claw, then its strum (`shape_notes_tracks`).
pub fn notes_tracks(sh: &Value, ppq: u32, keys: i64) -> (Vec<Note4>, Option<Vec<i64>>) {
    let (notes, tracks) = raw_notes_tracks(sh, ppq, keys);
    let claw = sh.get("claw").filter(|c| c.truthy());
    let strum = sh.get("strum").filter(|c| c.truthy());
    if claw.is_none() && strum.is_none() {
        return (notes, tracks);
    }
    let has_tracks = tracks.is_some();
    let mut rows: Vec<after::Row> = notes
        .iter()
        .enumerate()
        .map(|(i, r)| [r[0], r[1], r[2], r[3], tracks.as_ref().map_or(0, |t| t[i])])
        .collect();
    if let Some(c) = claw {
        rows = after::apply_claw(rows, c, ppq);
    }
    if let Some(s) = strum {
        rows = after::apply_strum(rows, s, ppq);
    }
    let notes = rows.iter().map(|r| [r[0], r[1], r[2], r[3]]).collect();
    let tracks = has_tracks.then(|| rows.iter().map(|r| r[4]).collect());
    (notes, tracks)
}

/// All notes of one shape.
pub fn shape_notes(sh: &Value, ppq: u32, keys: i64) -> Vec<Note4> {
    notes_tracks(sh, ppq, keys).0
}

/// Most colours (tracks) one shape keeps (a MIDI player's 15 note colours).
pub const COLOURS: usize = 15;

/// "Colours" are on (the notes take turns over channels).
pub fn cycling(sh: &Value) -> bool {
    sh.get("cycle").is_some_and(Value::truthy) && !sh.has("notes")
}

/// Whether the shape's tracks must get channels of their own (Multi channel).
pub fn tracks_apart(sh: &Value) -> bool {
    if kind(sh) != "custom" {
        return cycling(sh);
    }
    custom::Custom::new(sh, 960).tracks_apart()
}

/// A shape's groups kept to [`COLOURS`] colours: groups past the 15th are
/// merged into the 15th.
pub fn capped_colours(groups: &[i64]) -> Vec<i64> {
    let mut ids: Vec<i64> = groups.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() <= COLOURS {
        return groups.to_vec();
    }
    let cap = ids[COLOURS - 1];
    groups.iter().map(|g| (*g).min(cap)).collect()
}
