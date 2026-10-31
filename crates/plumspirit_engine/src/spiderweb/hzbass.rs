//! Hz bass (hzbass.py): spam so fast that the repeats sound like a tone. A
//! custom shape with `sh["hz"]` gets its spam gates from a tone; placed tones
//! (with slides) make the repeats every key is chopped by; effects give
//! every key its own repeats (`KeyGrid`) and change velocities.

use super::np::interp;
use super::rng::NpRng;
use super::{fnum, inum};
use crate::json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;

pub const MIN_LEN: f64 = 1.0 / 1024.0;
pub const TUNE: f64 = 50.0;
pub const AUTO: f64 = 3.0;
pub const AUTO_MOST: f64 = 50.0;
pub const VEL_FX: [&str; 5] = ["volume", "sweep", "wah", "tremolo", "octave"];
pub const WAVES: [&str; 4] = ["sine", "square", "saw", "triangle"];
pub const FX: [&str; 15] = [
    "slant", "groups", "offpitch", "noisy", "vibrato", "pitch", "volume", "sweep", "wah",
    "tremolo", "octave", "sine", "square", "saw", "triangle",
];
pub const SUB: usize = 16;
pub const SOFT: f64 = 0.003;
pub const VIBRATO: f64 = 0.05;
pub const VIBRATO_RATE: f64 = 2.5;
pub const WAH: f64 = 8.0;
pub const TREMOLO: f64 = 8.0;
pub const BEND: f64 = 0.98;
pub const LOOP: (f64, f64) = (1.0 / 256.0, 1024.0);
pub const GROUPS: i64 = 6;
pub const PITCH: f64 = 12.0;
pub const OFF_PITCH: f64 = 0.02;

/// Python `max(a, b)` / `min(a, b)` for floats (NaN rules included).
#[inline]
fn pmax(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}
#[inline]
fn pmin(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

fn neutral(name: &str) -> f64 {
    match name {
        "volume" => 1.0,
        "pitch" => 0.5,
        _ => 0.0,
    }
}

/// A line's point: beat, value, bend to the next (0 none, NaN "hold").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f64,
    pub v: f64,
    pub bend: f64,
}

#[derive(Clone, Debug)]
pub struct Slide {
    pub id: i64,
    pub out: f64,
    pub inn: f64,
}

#[derive(Clone, Debug)]
pub struct Tone {
    pub t: f64,
    pub len: f64,
    pub key: i64,
    pub cents: f64,
    pub id: i64,
    pub auto: Option<f64>,
    pub to: Vec<Slide>,
}

/// A cleaned `sh["hz"]`.
#[derive(Clone, Debug, Default)]
pub struct Hz {
    pub key: i64,
    pub cents: f64,
    pub bpm: f64,
    pub fixed: bool,
    pub auto: Option<f64>,
    pub fx: Vec<(String, Vec<Pt>)>,
    pub loops: Vec<(String, f64)>,
    pub off: Vec<String>,
    pub amount: Vec<(String, Vec<Pt>)>,
    pub tones: Vec<Tone>,
    pub grow: bool,
    pub own: bool,
}

fn get<'a, T>(v: &'a [(String, T)], name: &str) -> Option<&'a T> {
    v.iter().find(|(n, _)| n == name).map(|(_, t)| t)
}

/// `clean_point`: Err = the effect is dropped (Python raised), Ok(None) =
/// the point is.
fn clean_point(p: &Value) -> Result<Option<Pt>, ()> {
    let a = p.as_arr().ok_or(())?;
    if a.len() < 2 {
        return Err(());
    }
    let (u, v) = (fnum(&a[0]).ok_or(())?, fnum(&a[1]).ok_or(())?);
    if !(u.is_finite() && v.is_finite()) {
        return Ok(None);
    }
    let mut out = Pt {
        x: pmax(0.0, u),
        v: pmin(1.0, pmax(0.0, v)),
        bend: 0.0,
    };
    if a.len() > 2 {
        if a[2].as_str() == Some("hold") {
            out.bend = f64::NAN;
        } else {
            let b = fnum(&a[2]).ok_or(())?;
            if b.is_finite() && b.abs() > 1e-9 {
                out.bend = pmin(BEND, pmax(-BEND, b));
            }
        }
    }
    Ok(Some(out))
}

fn clean_fx(fx: &Value) -> Vec<(String, Vec<Pt>)> {
    let mut out = Vec::new();
    for name in FX {
        let pts: Vec<Pt> = match fx.get(name).filter(|v| v.truthy()) {
            None => Vec::new(),
            Some(list) => {
                let Some(list) = list.as_arr() else { continue };
                let got: Result<Vec<Option<Pt>>, ()> = list.iter().map(clean_point).collect();
                let Ok(got) = got else { continue };
                let mut pts: Vec<Pt> = got.into_iter().flatten().collect();
                pts.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
                pts
            }
        };
        if !pts.is_empty() {
            out.push((name.to_owned(), pts));
        }
    }
    out
}

fn clean_tones(tones: Option<&Value>) -> Vec<Tone> {
    let mut out: Vec<Tone> = Vec::new();
    let mut old: Vec<Option<(f64, f64)>> = Vec::new();
    let list = tones.and_then(Value::as_arr).unwrap_or(&[]);
    for n in list {
        let read = || -> Option<(Tone, (f64, f64))> {
            if !matches!(n, Value::Obj(_)) {
                return None;
            }
            let f = |k: &str, d: Option<f64>| -> Option<f64> {
                match n.get(k) {
                    None => d,
                    Some(v) => fnum(v),
                }
            };
            let t = pmax(0.0, f("t", None)?);
            let len = pmax(MIN_LEN, f("len", None)?);
            let key = inum(n.get("key")?)?;
            let cents = pmax(-TUNE, pmin(TUNE, f("cents", Some(0.0))?));
            let leads = (
                pmax(0.0, f("in", Some(0.0))?),
                pmax(0.0, f("out", Some(0.0))?),
            );
            let id = match n.get("id") {
                None => 0,
                Some(v) => inum(v)?,
            };
            let auto = match n.get("auto") {
                None | Some(Value::Null) => None,
                Some(v) => Some(pmax(0.0, pmin(AUTO_MOST, fnum(v)?))),
            };
            let mut to = Vec::new();
            if let Some(list) = n.get("to").filter(|v| v.truthy()) {
                for s in list.as_arr()? {
                    to.push(Slide {
                        id: inum(s.get("id")?)?,
                        out: pmax(0.0, fnum(s.get("out")?)?),
                        inn: pmax(0.0, fnum(s.get("in")?)?),
                    });
                }
            }
            Some((
                Tone {
                    t,
                    len,
                    key,
                    cents,
                    id,
                    auto,
                    to,
                },
                leads,
            ))
        };
        let Some((tone, leads)) = read() else {
            continue;
        };
        let ok = [
            tone.t,
            tone.len,
            tone.cents,
            tone.auto.unwrap_or(0.0),
            leads.0,
            leads.1,
        ]
        .iter()
        .chain(tone.to.iter().flat_map(|s| [&s.out, &s.inn]))
        .all(|v| v.is_finite());
        // Keys 0–255 (v0.9 256-Key Mode); the board's keyboard clips the
        // notes (`raw_notes_tracks`), so a standard project plays 0–127.
        if (0..=255).contains(&tone.key) && ok {
            out.push(tone);
            old.push(if n.has("id") { None } else { Some(leads) });
        }
    }
    let mut order: Vec<usize> = (0..out.len()).collect();
    order.sort_by(|&a, &b| {
        out[a]
            .t
            .partial_cmp(&out[b].t)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(out[a].key.cmp(&out[b].key))
    });
    let mut out: Vec<Tone> = order.iter().map(|&i| out[i].clone()).collect();
    let old: Vec<Option<(f64, f64)>> = order.iter().map(|&i| old[i]).collect();
    let mut seen: Vec<i64> = Vec::new();
    for i in 0..out.len() {
        if out[i].id <= 0 || seen.contains(&out[i].id) {
            out[i].id = out.iter().map(|m| m.id).chain([0]).max().unwrap_or(0) + 1;
        }
        seen.push(out[i].id);
    }
    // saved before slides were their own thing: it slid to the next tone on its line
    let mut lines: Vec<Vec<(usize, (f64, f64))>> = Vec::new();
    for (i, leads) in old.iter().enumerate() {
        let Some(leads) = *leads else { continue };
        let mut placed = false;
        for line in &mut lines {
            let (a, a_leads) = *line.last().expect("line");
            if out[a].t + out[a].len <= out[i].t + 1e-9 {
                if a_leads.1 > 0.0 || leads.0 > 0.0 {
                    let id = out[i].id;
                    out[a].to.push(Slide {
                        id,
                        out: a_leads.1,
                        inn: leads.0,
                    });
                }
                line.push((i, leads));
                placed = true;
                break;
            }
        }
        if !placed {
            lines.push(vec![(i, leads)]);
        }
    }
    for n in &mut out {
        let mut kept: Vec<Slide> = Vec::new();
        for s in &n.to {
            if seen.contains(&s.id) && s.id != n.id && !kept.iter().any(|k| k.id == s.id) {
                kept.push(s.clone());
            }
        }
        n.to = kept;
    }
    out
}

fn old_fx(tones: Option<&Value>) -> Vec<(String, Vec<Pt>)> {
    let list = tones.and_then(Value::as_arr).unwrap_or(&[]);
    let mut with: Vec<&Value> = list
        .iter()
        .filter(|n| matches!(n.get("fx"), Some(Value::Obj(_))))
        .collect();
    with.sort_by(|a, b| {
        let ta = a.get("t").and_then(fnum).unwrap_or(0.0);
        let tb = b.get("t").and_then(fnum).unwrap_or(0.0);
        ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out: Vec<(String, Vec<Value>)> = Vec::new();
    for n in with {
        let (Some(t), Some(span)) = (n.get("t").and_then(fnum), n.get("len").and_then(fnum)) else {
            continue;
        };
        for (name, pts) in clean_fx(n.get("fx").expect("fx")) {
            let e = match out.iter().position(|(k, _)| *k == name) {
                Some(i) => i,
                None => {
                    out.push((name.clone(), Vec::new()));
                    out.len() - 1
                }
            };
            for p in pts {
                out[e].1.push(pt_value(&Pt {
                    x: t + pmin(1.0, p.x) * span,
                    ..p
                }));
            }
        }
    }
    let mut obj = Value::obj();
    for (k, v) in out {
        obj.set(&k, Value::Arr(v));
    }
    clean_fx(&obj)
}

fn pt_value(p: &Pt) -> Value {
    let mut v = vec![Value::Num(p.x), Value::Num(p.v)];
    if p.bend.is_nan() {
        v.push(Value::Str("hold".into()));
    } else if p.bend != 0.0 {
        v.push(Value::Num(p.bend));
    }
    Value::Arr(v)
}

fn line_value(pts: &[Pt]) -> Value {
    Value::Arr(pts.iter().map(pt_value).collect())
}

impl Hz {
    /// A saved `hz` checked (`clean_hz`), or None.
    pub fn clean(hz: Option<&Value>) -> Option<Hz> {
        let hz = hz?;
        if !matches!(hz, Value::Obj(_)) {
            return None;
        }
        let key = inum(hz.get("key")?)?;
        let cents = match hz.get("cents") {
            None => 0.0,
            Some(v) => fnum(v)?,
        };
        let bpm = fnum(hz.get("bpm")?)?;
        if !((0..=255).contains(&key) && cents.abs() <= 1200.0 && bpm > 0.0) {
            return None;
        }
        let mut out = Hz {
            key,
            cents,
            bpm,
            ..Hz::default()
        };
        if hz.get("fixed") == Some(&Value::Bool(true)) {
            out.fixed = true;
        } else {
            let auto = match hz.get("auto") {
                None | Some(Value::Null) => None,
                Some(v) => fnum(v),
            };
            if let Some(a) = auto
                && a.is_finite()
            {
                out.auto = Some(pmax(0.0, pmin(AUTO_MOST, a)));
            }
        }
        let tones = clean_tones(hz.get("tones"));
        let fx = match hz.get("fx") {
            Some(f @ Value::Obj(_)) => clean_fx(f),
            _ => old_fx(hz.get("tones").filter(|t| t.truthy())),
        };
        if !fx.is_empty() {
            out.fx = fx;
            if let Some(Value::Obj(l)) = hz.get("loop") {
                for (name, every) in l {
                    let Some(every) = fnum(every) else { continue };
                    if let Some(i) = out.fx.iter().position(|(n, _)| n == name)
                        && every.is_finite()
                        && LOOP.0 <= every
                        && every <= LOOP.1
                    {
                        match out.loops.iter_mut().find(|(n, _)| n == name) {
                            Some(e) => e.1 = every,
                            None => out.loops.push((name.clone(), every)),
                        }
                        for p in &mut out.fx[i].1 {
                            p.x = pmin(p.x, every);
                        }
                    }
                }
            }
            if let Some(off) = hz.get("off").and_then(Value::as_arr) {
                out.off = FX
                    .iter()
                    .filter(|n| {
                        get(&out.fx, n).is_some() && off.iter().any(|o| o.as_str() == Some(n))
                    })
                    .map(|n| (*n).to_owned())
                    .collect();
            }
            if let Some(a @ Value::Obj(_)) = hz.get("amount") {
                out.amount = clean_fx(a)
                    .into_iter()
                    .filter(|(n, _)| get(&out.loops, n).is_some())
                    .collect();
            }
        }
        if !tones.is_empty() {
            out.tones = tones;
            out.grow = hz.get("grow") == Some(&Value::Bool(true));
            out.own = hz.get("own") == Some(&Value::Bool(true));
        }
        Some(out)
    }

    pub fn to_value(&self) -> Value {
        let mut o = Value::obj();
        o.set("key", Value::Num(self.key as f64));
        o.set("cents", Value::Num(self.cents));
        o.set("bpm", Value::Num(self.bpm));
        if self.fixed {
            o.set("fixed", Value::Bool(true));
        }
        if let Some(a) = self.auto {
            o.set("auto", Value::Num(a));
        }
        let lines = |v: &[(String, Vec<Pt>)]| {
            let mut f = Value::obj();
            for (n, pts) in v {
                f.set(n, line_value(pts));
            }
            f
        };
        if !self.fx.is_empty() {
            o.set("fx", lines(&self.fx));
            if !self.loops.is_empty() {
                let mut l = Value::obj();
                for (n, e) in &self.loops {
                    l.set(n, Value::Num(*e));
                }
                o.set("loop", l);
            }
            if !self.off.is_empty() {
                o.set(
                    "off",
                    Value::Arr(self.off.iter().map(|s| Value::Str(s.clone())).collect()),
                );
            }
            if !self.amount.is_empty() {
                o.set("amount", lines(&self.amount));
            }
        }
        if !self.tones.is_empty() {
            let tones = self
                .tones
                .iter()
                .map(|n| {
                    let mut t = Value::obj();
                    t.set("t", Value::Num(n.t));
                    t.set("len", Value::Num(n.len));
                    t.set("key", Value::Num(n.key as f64));
                    t.set("cents", Value::Num(n.cents));
                    t.set("id", Value::Num(n.id as f64));
                    if let Some(a) = n.auto {
                        t.set("auto", Value::Num(a));
                    }
                    t.set(
                        "to",
                        Value::Arr(
                            n.to.iter()
                                .map(|s| {
                                    let mut v = Value::obj();
                                    v.set("id", Value::Num(s.id as f64));
                                    v.set("out", Value::Num(s.out));
                                    v.set("in", Value::Num(s.inn));
                                    v
                                })
                                .collect(),
                        ),
                    );
                    t
                })
                .collect();
            o.set("tones", Value::Arr(tones));
            if self.grow {
                o.set("grow", Value::Bool(true));
            }
            if self.own {
                o.set("own", Value::Bool(true));
            }
        }
        o
    }

    /// As it's heard: without the effects switched off (Bypass).
    pub fn live(&self) -> Hz {
        if self.off.is_empty() {
            return self.clone();
        }
        let mut h = self.clone();
        h.fx.retain(|(n, _)| !self.off.contains(n));
        h.loops.retain(|(n, _)| get(&h.fx, n).is_some());
        h.amount.retain(|(n, _)| get(&h.loops, n).is_some());
        h.off.clear();
        h
    }

    pub fn has_fx(&self) -> bool {
        !self.fx.is_empty() && !self.tones.is_empty()
    }

    /// Auto gates' threshold in cents for tone n (its own, or the Hz bass's).
    pub fn threshold(&self, n: Option<&Tone>) -> Option<f64> {
        let a = self.auto?;
        Some(n.and_then(|n| n.auto).unwrap_or(a))
    }

    /// An effect's value at these beats (0 without its line).
    pub fn fx_at(&self, name: &str, beat: &[f64]) -> Vec<f64> {
        let Some(pts) = get(&self.fx, name) else {
            return vec![0.0; beat.len()];
        };
        let every = get(&self.loops, name).copied();
        let mut v = line_at(pts, beat, every);
        if every.is_some()
            && let Some(amount) = get(&self.amount, name)
        {
            let still = neutral(name);
            let a = line_at(amount, beat, None);
            for (x, y) in v.iter_mut().zip(a) {
                *x = still + (*x - still) * y;
            }
        }
        v
    }

    /// The gate, in ticks, of one wave of key's tone.
    pub fn wave(&self, ppq: f64, key: f64, limit: Option<f64>) -> f64 {
        let gate = ppq * self.bpm / 60.0 / hz_of(key, self.cents);
        let whole = self.fixed || limit.is_some_and(|l| off_cents(gate) <= l + 1e-9);
        pmax(1.0, if whole { (gate + 0.5).floor() } else { gate })
    }
}

/// The tone of a key in Hz (key 69 = 440 Hz).
pub fn hz_of(key: f64, cents: f64) -> f64 {
    440.0 * 2.0f64.powf((key - 69.0 + cents / 100.0) / 12.0)
}

/// The spam gate, in beats, that sounds like the tone at this BPM.
pub fn hz_gate(key: f64, cents: f64, bpm: f64) -> f64 {
    let g = bpm / (60.0 * hz_of(key, cents));
    let g: f64 = format!("{g:.11e}").parse().unwrap_or(g);
    pmax(1e-6, g)
}

/// How far, in cents, a gate rounded to a whole tick is off the exact one.
pub fn off_cents(gate: f64) -> f64 {
    let whole = pmax(1.0, (gate + 0.5).floor());
    (1200.0 * (gate / whole).log2()).abs()
}

fn pitch(n: &Tone) -> f64 {
    n.key as f64 + n.cents / 100.0
}

fn np_mod(a: f64, b: f64) -> f64 {
    let m = a % b;
    if m != 0.0 {
        if (b < 0.0) != (m < 0.0) { m + b } else { m }
    } else {
        0.0f64.copysign(b)
    }
}

/// How far along (0..1) a bent line has come at u.
fn bent_part(u: f64, bend: f64) -> f64 {
    if bend.is_nan() {
        return if u >= 1.0 { 1.0 } else { 0.0 };
    }
    let m = (1.0 + bend) / 2.0;
    if bend > 0.0 {
        1.0 - (1.0 - u).powf((1.0 - m).ln() / 0.5f64.ln())
    } else if bend < 0.0 {
        u.powf(m.ln() / 0.5f64.ln())
    } else {
        u
    }
}

/// A line's value at each beat (`line_at`).
pub fn line_at(pts: &[Pt], beat: &[f64], every: Option<f64>) -> Vec<f64> {
    let mut p: Vec<Pt> = pts.to_vec();
    let every = every.filter(|e| *e != 0.0);
    if let Some(e) = every {
        let (l, f) = (pts[pts.len() - 1], pts[0]);
        p.insert(0, Pt { x: l.x - e, ..l });
        p.push(Pt { x: f.x + e, ..f });
    }
    if p.len() == 1 {
        return vec![p[0].v; beat.len()];
    }
    let last = p.len() - 2;
    beat.iter()
        .map(|&b| {
            let b = match every {
                Some(e) => np_mod(b, e),
                None => b,
            };
            let j = p.partition_point(|q| q.x <= b) as i64 - 1;
            let j = j.clamp(0, last as i64) as usize;
            let w = p[j + 1].x - p[j].x;
            let u = if w > 0.0 {
                ((b - p[j].x) / w).clamp(0.0, 1.0)
            } else {
                1.0
            };
            p[j].v + (p[j + 1].v - p[j].v) * bent_part(u, p[j].bend)
        })
        .collect()
}

/// One stretch of tone's repeats: start ticks, next ones' starts, whose
/// (the tone held, or the slide from / to), and the end of a held tone.
struct Run {
    starts: Vec<f64>,
    nexts: Vec<f64>,
    /// tone index held, or the slide's two tone indices
    held: Option<usize>,
}

fn links(tones: &[Tone]) -> Vec<(usize, usize, Slide)> {
    let mut out = Vec::new();
    for (ai, a) in tones.iter().enumerate() {
        for s in &a.to {
            if let Some(bi) = tones.iter().position(|b| b.id == s.id)
                && bi != ai
                && tones[bi].t >= a.t + a.len - 1e-9
            {
                out.push((ai, bi, s.clone()));
            }
        }
    }
    out
}

fn tone_runs(hz: &Hz, left: f64, ppq: f64) -> Vec<Run> {
    let tones = &hz.tones;
    let ls = links(tones);
    let mut out = Vec::new();
    let mut held: HashMap<i64, (f64, f64)> = HashMap::new();
    for (ni, n) in tones.iter().enumerate() {
        let lead_in = ls
            .iter()
            .filter(|(_, b, _)| *b == ni)
            .map(|(_, _, s)| pmin(s.inn, n.len))
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| pmin(m, v))))
            .unwrap_or(0.0);
        let lead_out = ls
            .iter()
            .filter(|(a, _, _)| *a == ni)
            .map(|(_, _, s)| pmin(s.out, n.len))
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| pmin(m, v))))
            .unwrap_or(0.0);
        let a = n.t + lead_in;
        let b = n.t + n.len - lead_out;
        if b > a {
            let (s, e) = ((left + a) * ppq, (left + b) * ppq);
            let gate = hz.wave(ppq, pitch(n), hz.threshold(Some(n)));
            let count = ((e - s) / gate).ceil() as i64;
            let starts: Vec<f64> = (0..count.max(0)).map(|i| s + gate * i as f64).collect();
            let nexts = starts.iter().map(|x| x + gate).collect();
            out.push(Run {
                starts,
                nexts,
                held: Some(ni),
            });
            held.insert(n.id, (s, gate));
        }
    }
    for (ai, bi, link) in &ls {
        let (a, b) = (&tones[*ai], &tones[*bi]);
        let x0 = a.t + a.len - pmin(link.out, a.len);
        let x1 = b.t + pmin(link.inn, b.len);
        let (k0, k1) = (pitch(a), pitch(b));
        if x1 - x0 < 1e-12 {
            continue;
        }
        let (s0, e0) = ((left + x0) * ppq, (left + a.t + a.len) * ppq);
        let (s1, e1) = ((left + b.t) * ppq, (left + x1) * ppq);
        let mut t = s0;
        if let Some(&(s, gate)) = held.get(&a.id)
            && s0 >= s
        {
            t = s + ((s0 - s) / gate - 1e-9).ceil() * gate;
        }
        let gap = s1 - e0 > 1e-6;
        let ends: &[f64] = if gap { &[e0, e1] } else { &[e1] };
        for &end in ends {
            let (mut part, mut after) = (Vec::new(), Vec::new());
            while t < end {
                part.push(t);
                t += hz.wave(ppq, k0 + (k1 - k0) * (t - s0) / (e1 - s0), None);
                after.push(t);
            }
            if !part.is_empty() {
                out.push(Run {
                    starts: part,
                    nexts: after,
                    held: None,
                });
            }
            t = pmax(t, s1);
        }
    }
    if get(&hz.fx, "pitch").is_some() {
        for r in &mut out {
            let (a, b) = bent(hz, left, ppq, &r.starts, &r.nexts);
            r.starts = a;
            r.nexts = b;
        }
    }
    out
}

fn bent(hz: &Hz, left: f64, ppq: f64, starts: &[f64], nexts: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let mut t: Vec<f64> = starts.to_vec();
    t.push(nexts[nexts.len() - 1]);
    let beats: Vec<f64> = t.iter().map(|x| x / ppq - left).collect();
    let f: Vec<f64> = hz
        .fx_at("pitch", &beats)
        .iter()
        .map(|v| 2.0f64.powf((v - 0.5) * 2.0 * PITCH / 12.0))
        .collect();
    let mut phase = vec![0.0];
    let mut acc = 0.0;
    for w in f.windows(2) {
        acc += (w[0] + w[1]) / 2.0;
        phase.push(acc);
    }
    let ph_last = phase[phase.len() - 1];
    let n = ((ph_last - 1e-9).ceil() as i64).max(1);
    let (t_last, f_last) = (t[t.len() - 1], f[f.len() - 1]);
    let wave = nexts[nexts.len() - 1] - starts[starts.len() - 1];
    let at: Vec<f64> = (0..=n)
        .map(|i| {
            let x = i as f64;
            if x > ph_last {
                t_last + (x - ph_last) * wave / f_last
            } else {
                interp(x, &phase, &t)
            }
        })
        .collect();
    (at[..at.len() - 1].to_vec(), at[1..].to_vec())
}

/// For each repeat: the tick the sound it's in ends at.
fn limits(tones: &[Tone], left: f64, ppq: f64, starts: &[f64]) -> Vec<i64> {
    let mut order: Vec<&Tone> = tones.iter().collect();
    order.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    let mut spans: Vec<[f64; 2]> = Vec::new();
    for n in order {
        if let Some(l) = spans.last_mut()
            && n.t <= l[1] + 1e-9
        {
            l[1] = pmax(l[1], n.t + n.len);
            continue;
        }
        spans.push([n.t, n.t + n.len]);
    }
    let at: Vec<f64> = spans.iter().map(|s| (left + s[0]) * ppq - 1e-6).collect();
    let ends: Vec<i64> = spans
        .iter()
        .map(|s| ((left + s[1]) * ppq + 0.5).floor() as i64)
        .collect();
    starts
        .iter()
        .map(|&s| {
            let i = (at.partition_point(|a| *a <= s) as i64 - 1).max(0) as usize;
            ends[i]
        })
        .collect()
}

fn whole(v: f64) -> i64 {
    (v + 0.5).floor() as i64
}

/// Repeats → `(start, end)` whole ticks in order, each lasting until the
/// next one starts; and which repeat each of them is.
fn grid(starts: &[f64], lims: &[i64]) -> (Vec<[i64; 2]>, Vec<usize>) {
    if starts.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let s: Vec<i64> = starts.iter().map(|&v| whole(v)).collect();
    let mut order: Vec<usize> = (0..s.len()).collect();
    order.sort_by_key(|&i| s[i]);
    let mut st: Vec<i64> = Vec::new();
    let mut li: Vec<i64> = Vec::new();
    let mut who: Vec<usize> = Vec::new();
    for &o in &order {
        if st.last() == Some(&s[o]) {
            let l = li.last_mut().expect("open");
            *l = (*l).max(lims[o]);
        } else {
            st.push(s[o]);
            li.push(lims[o]);
            who.push(o);
        }
    }
    let mut out = Vec::new();
    let mut which = Vec::new();
    for i in 0..st.len() {
        let next = if i + 1 < st.len() {
            st[i + 1]
        } else {
            li[li.len() - 1]
        };
        let e = next.min(li[i]);
        if e > st[i] {
            out.push([st[i], e]);
            which.push(who[i]);
        }
    }
    (out, which)
}

/// The repeats of placed tones (no effects): `(start, end)` ticks in order.
pub fn plain_squares(hz: &Hz, left: f64, ppq: f64) -> Vec<[i64; 2]> {
    let runs = tone_runs(hz, left, ppq);
    let starts: Vec<f64> = runs.iter().flat_map(|r| r.starts.iter().copied()).collect();
    if starts.is_empty() {
        return Vec::new();
    }
    let l = limits(&hz.tones, left, ppq, &starts);
    grid(&starts, &l).0
}

/// A stretch of tone with its effects worked out per repeat.
#[derive(Clone)]
struct GridRun {
    starts: Vec<f64>,
    waves: Vec<f64>,
    number: Vec<i64>,
    beat: Vec<f64>,
    limits: Vec<i64>,
    until: f64,
    fx: HashMap<&'static str, Vec<f64>>,
    groups: Vec<i64>,
    turns: Vec<f64>,
    has: HashMap<&'static str, bool>,
}

/// A key's repeats and each one's part of the velocity.
type Made = (Vec<[i64; 2]>, Vec<f64>);

/// The repeats of a Hz bass with effects: every key has its own, and how
/// much of the shape's velocity each gets.
#[derive(Clone)]
pub struct KeyGrid {
    lo: i64,
    n: i64,
    runs: Vec<GridRun>,
    shaped: bool,
    pub loud: bool,
    got: RefCell<HashMap<i64, Made>>,
}

impl std::fmt::Debug for KeyGrid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KeyGrid({} runs)", self.runs.len())
    }
}

impl KeyGrid {
    pub fn new(hz: &Hz, left: f64, ppq: f64, lo: i64, n: i64) -> KeyGrid {
        let mut runs = Vec::new();
        for r in tone_runs(hz, left, ppq) {
            let beat: Vec<f64> = r.starts.iter().map(|s| s / ppq - left).collect();
            let waves: Vec<f64> = r.nexts.iter().zip(&r.starts).map(|(a, b)| a - b).collect();
            let until = match r.held {
                Some(i) => (left + hz.tones[i].t + hz.tones[i].len) * ppq,
                None => f64::INFINITY,
            };
            let mut fx = HashMap::new();
            for name in FX {
                fx.insert(name, hz.fx_at(name, &beat));
            }
            let mut has = HashMap::new();
            has.insert("sweep", get(&hz.fx, "sweep").is_some());
            has.insert("volume", get(&hz.fx, "volume").is_some());
            for name in WAVES {
                has.insert(name, get(&hz.fx, name).is_some());
            }
            let groups = fx["groups"]
                .iter()
                .map(|v| 1 + (v * (GROUPS - 1) as f64 + 0.5).floor() as i64)
                .collect();
            let mut turns = vec![0.0];
            let mut acc = 0.0;
            for (tr, w) in fx["tremolo"].iter().zip(&waves) {
                acc += tr * TREMOLO * w / ppq;
                turns.push(acc);
            }
            turns.pop();
            runs.push(GridRun {
                limits: limits(&hz.tones, left, ppq, &r.starts),
                number: (0..r.starts.len() as i64).collect(),
                starts: r.starts,
                waves,
                beat,
                until,
                fx,
                groups,
                turns,
                has,
            });
        }
        let shaped = runs
            .iter()
            .any(|r| WAVES.iter().any(|w| r.has[w] && !r.starts.is_empty()));
        let loud = shaped || VEL_FX.iter().any(|n| get(&hz.fx, n).is_some());
        KeyGrid {
            lo,
            n: n.max(1),
            runs,
            shaped,
            loud,
            got: RefCell::new(HashMap::new()),
        }
    }

    fn respaced(run: &GridRun, x: f64) -> Option<GridRun> {
        let (off, vib) = (&run.fx["offpitch"], &run.fx["vibrato"]);
        if !(off.iter().any(|v| *v != 0.0) || vib.iter().any(|v| *v != 0.0)) {
            return None;
        }
        let b0 = run.beat[0];
        let stretch: Vec<f64> = (0..off.len())
            .map(|i| {
                (1.0 + OFF_PITCH * off[i] * (x - 0.5))
                    * (1.0
                        + VIBRATO * vib[i] * (2.0 * PI * VIBRATO_RATE * (run.beat[i] - b0)).sin())
            })
            .collect();
        let n = off.len();
        let more = n / 10 + 3;
        let pick: Vec<usize> = (0..n + more).map(|i| i.min(n - 1)).collect();
        let waves: Vec<f64> = pick.iter().map(|&i| run.waves[i] * stretch[i]).collect();
        let sel = |v: &Vec<f64>| -> Vec<f64> { pick.iter().map(|&i| v[i]).collect() };
        let mut out = run.clone();
        out.beat = sel(&run.beat);
        out.limits = pick.iter().map(|&i| run.limits[i]).collect();
        out.groups = pick.iter().map(|&i| run.groups[i]).collect();
        out.turns = sel(&run.turns);
        out.fx = run.fx.iter().map(|(k, v)| (*k, sel(v))).collect();
        let mut starts = vec![run.starts[0]];
        let mut acc = 0.0;
        for w in &waves[..waves.len() - 1] {
            acc += w;
            starts.push(run.starts[0] + acc);
        }
        out.starts = starts;
        out.waves = waves;
        out.number = (0..(n + more) as i64).collect();
        Some(out)
    }

    fn made(&self, key: i64) -> (Vec<[i64; 2]>, Vec<f64>) {
        if let Some(g) = self.got.borrow().get(&key) {
            return g.clone();
        }
        let x = pmin(1.0, pmax(0.0, (key - self.lo) as f64 / self.n as f64));
        let xv = pmin(
            1.0,
            pmax(0.0, (key - self.lo) as f64 / (self.n - 1).max(1) as f64),
        );
        let mut noise = NpRng::new((1000 + key) as u64);
        let (mut all_s, mut all_l, mut all_f, mut all_q) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for run0 in &self.runs {
            let sp = Self::respaced(run0, x);
            let run = sp.as_ref().unwrap_or(run0);
            let m = run.starts.len();
            let fx = |n: &str| &run.fx[n];
            let mut late: Vec<f64> = (0..m)
                .map(|i| {
                    let g = run.groups[i] as f64;
                    fx("slant")[i] * x + (x * g).floor() / g
                })
                .collect();
            if fx("noisy").iter().any(|v| *v != 0.0) {
                for (l, nz) in late.iter_mut().zip(fx("noisy")) {
                    *l += nz * noise.random();
                }
            }
            let mut starts: Vec<f64> = (0..m)
                .map(|i| run.starts[i] + late[i] * run.waves[i])
                .collect();
            let mut lims = run.limits.clone();
            let mut factor = vec![1.0; m];
            let mut quiet = vec![false; m];
            if self.loud {
                let mut loud = vec![0.0; m];
                let mut vol = vec![0.0; m];
                let mut soft = vec![0.0; m];
                for i in 0..m {
                    let mut l = if run.has["sweep"] {
                        0.08 + 0.92 * (PI * (xv - fx("sweep")[i])).cos().clamp(0.0, 1.0).powf(4.0)
                    } else {
                        1.0
                    };
                    l = l * (1.0 + (2.0 * PI * WAH * fx("wah")[i] * (xv - 0.5)).cos()) / 2.0;
                    l *= 0.1 + 0.9 * (1.0 + (2.0 * PI * run.turns[i]).cos()) / 2.0;
                    vol[i] = if run.has["volume"] {
                        fx("volume")[i]
                    } else {
                        1.0
                    };
                    loud[i] = l * vol[i];
                    soft[i] = if run.number[i].rem_euclid(2) == 1 {
                        1.0 - fx("octave")[i]
                    } else {
                        1.0
                    };
                }
                if self.shaped {
                    let part: Vec<f64> = (0..SUB).map(|j| j as f64 / SUB as f64).collect();
                    let (mut ns, mut nl, mut nf, mut nq) =
                        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                    for i in 0..m {
                        let plain = !WAVES.iter().any(|w| run.has[w]);
                        for (j, &p) in part.iter().enumerate() {
                            let mut mix = 1.0f64;
                            if plain {
                                mix = if j == 0 { 1.0 } else { 0.0 };
                            } else {
                                for w in WAVES {
                                    if run.has[w] {
                                        let v = fx(w)[i];
                                        let shape = match w {
                                            "sine" => (1.0 + (2.0 * PI * p).cos()) / 2.0,
                                            "square" => {
                                                if p < 0.5 {
                                                    1.0
                                                } else {
                                                    0.0
                                                }
                                            }
                                            "saw" => 1.0 - p,
                                            _ => 1.0 - (2.0 * p - 1.0).abs(),
                                        };
                                        mix *=
                                            (1.0 - v) * if j == 0 { 1.0 } else { 0.0 } + v * shape;
                                    }
                                }
                            }
                            if mix >= SOFT {
                                ns.push(starts[i] + p * run.waves[i]);
                                nl.push(lims[i]);
                                nf.push((loud[i] * mix).sqrt() * soft[i]);
                                nq.push(vol[i] < SOFT);
                            }
                        }
                    }
                    starts = ns;
                    lims = nl;
                    factor = nf;
                    quiet = nq;
                } else {
                    factor = (0..m).map(|i| loud[i].sqrt() * soft[i]).collect();
                    quiet = vol.iter().map(|v| *v < SOFT).collect();
                }
            }
            let cut = pmin(run.until, f64::INFINITY) - 1e-6;
            for i in 0..starts.len() {
                if starts[i] < cut {
                    all_s.push(starts[i]);
                    all_l.push(lims[i]);
                    all_f.push(factor[i]);
                    all_q.push(quiet[i]);
                }
            }
        }
        let (sq, f) = if all_s.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let (sq, which) = grid(&all_s, &all_l);
            let mut s2 = Vec::new();
            let mut f2 = Vec::new();
            for (q, w) in sq.into_iter().zip(which) {
                if !all_q[w] {
                    s2.push(q);
                    f2.push(all_f[w]);
                }
            }
            (s2, f2)
        };
        self.got.borrow_mut().insert(key, (sq.clone(), f.clone()));
        (sq, f)
    }

    /// A key's repeats: `(start, end)` ticks in order.
    pub fn squares(&self, key: i64) -> Vec<[i64; 2]> {
        self.made(key).0
    }

    /// What the effects make of the velocity of a key's notes (start ticks).
    pub fn factor(&self, key: i64, starts: &[i64]) -> Vec<f64> {
        let (sq, factor) = self.made(key);
        if sq.is_empty() {
            return vec![1.0; starts.len()];
        }
        starts
            .iter()
            .map(|&s| {
                let at = sq.partition_point(|q| q[0] < s).min(sq.len() - 1);
                if sq[at][0] == s { factor[at] } else { 1.0 }
            })
            .collect()
    }
}

/// The lowest and highest key of a custom shape's box (Python `round`).
pub fn key_range(pts: &[[f64; 2]]) -> (i64, i64) {
    let mut ps: Vec<f64> = pts.iter().map(|p| p[1]).collect();
    ps.push(ps[1] + ps[2] - ps[0]);
    let lo = ps.iter().copied().fold(f64::INFINITY, pmin);
    let hi = ps.iter().copied().fold(f64::NEG_INFINITY, pmax);
    (super::curves::rne(lo) as i64, super::curves::rne(hi) as i64)
}

/// The beat a custom shape's box starts at (its tones count from there).
pub fn left_edge(pts: &[[f64; 2]]) -> f64 {
    let (b0, b1, b2) = (pts[0][0], pts[1][0], pts[2][0]);
    pmin(pmin(pmin(b0, b1), b2), b1 + b2 - b0)
}

/// The repeats a shape with placed tones is chopped by.
pub enum Squares {
    Plain(Vec<[i64; 2]>),
    Keys(KeyGrid),
}

pub fn squares(hz: &Hz, pts: &[[f64; 2]], ppq: u32) -> Squares {
    let hz = hz.live();
    let ppq = f64::from(ppq);
    if hz.has_fx() {
        let (lo, hi) = key_range(pts);
        return Squares::Keys(KeyGrid::new(&hz, left_edge(pts), ppq, lo, hi - lo + 1));
    }
    Squares::Plain(plain_squares(&hz, left_edge(pts), ppq))
}

/// What the effects make of the velocities (None: no effect changes them).
pub fn velocity_factor(
    hz: &Hz,
    pts: &[[f64; 2]],
    ppq: u32,
    starts: &[i64],
    keys: &[i64],
) -> Option<Vec<f64>> {
    if !hz.live().has_fx() || starts.is_empty() {
        return None;
    }
    let Squares::Keys(grid) = squares(hz, pts, ppq) else {
        return None;
    };
    if !grid.loud {
        return None;
    }
    let mut out = vec![1.0; starts.len()];
    let mut ks: Vec<i64> = keys.to_vec();
    ks.sort_unstable();
    ks.dedup();
    for k in ks {
        let rows: Vec<usize> = (0..keys.len()).filter(|&i| keys[i] == k).collect();
        let s: Vec<i64> = rows.iter().map(|&i| starts[i]).collect();
        for (r, f) in rows.iter().zip(grid.factor(k, &s)) {
            out[*r] = f;
        }
    }
    Some(out)
}
