//! Joined curves (`notes/joined.py`, the note-relevant part): several line
//! shapes chained into one `curve` with pieces (`gaps`: segments not drawn)
//! and per-section tumours (`tumours`, sections split at `splits`).

use super::P;
use super::curves::{anchor_count, sample};
use super::pattern::formed_paths;
use super::tumour::{clean_tumour, tumour_path};
use super::{inum, pts_of};
use crate::json::Value;

/// Python `sorted({int(a) for a in xs})` (None: Python raises).
fn int_set(v: Option<&Value>) -> Option<Vec<i64>> {
    let mut out = Vec::new();
    if let Some(v) = v {
        for a in v.as_arr()? {
            out.push(inum(a)?);
        }
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

/// A joined curve's gaps / splits / tumours from `sh` into `out` (a cleaned
/// curve); anything that doesn't fit the curve is dropped.
pub fn clean_joined(sh: &Value, out: &mut Value) {
    let n = out.get("pts").and_then(pts_of).map_or(0, |p| p.len());
    let last = anchor_count(n) as i64 - 1;
    let Some(want) = int_set(sh.get("gaps")) else {
        return;
    };
    let Some(splits) = int_set(sh.get("splits")) else {
        return;
    };
    let mut gaps: Vec<i64> = Vec::new();
    for g in want {
        if 1 <= g && g <= last - 2 && gaps.last().is_none_or(|&p| g >= p + 2) {
            gaps.push(g);
        }
    }
    let splits: Vec<i64> = splits
        .into_iter()
        .filter(|&a| 0 < a && a < last && !gaps.contains(&a) && !gaps.contains(&(a - 1)))
        .collect();
    let nums = |v: &[i64]| Value::Arr(v.iter().map(|&a| Value::Num(a as f64)).collect());
    if !gaps.is_empty() {
        out.set("gaps", nums(&gaps));
    }
    if let Some(tms) = sh.get("tumours").and_then(Value::as_arr)
        && tms.len() == splits.len() + gaps.len() + 1
    {
        out.set(
            "tumours",
            Value::Arr(
                tms.iter()
                    .map(|t| clean_tumour(Some(t)).unwrap_or(Value::Null))
                    .collect(),
            ),
        );
        if !splits.is_empty() {
            out.set("splits", nums(&splits));
        }
        if let Value::Obj(o) = out {
            o.retain(|(k, _)| k != "tumour");
        }
    }
}

pub fn is_joined(sh: &Value) -> bool {
    sh.get("kind").and_then(Value::as_str) == Some("curve")
        && (sh.get("gaps").is_some_and(Value::truthy)
            || sh.get("tumours").is_some_and(Value::truthy))
}

/// The tumour settings shown: the shape's own, or on a joined curve with one
/// per section, the first there is.
pub fn shown_tumour(sh: &Value) -> Option<&Value> {
    if let Some(t) = sh.get("tumours").filter(|t| t.truthy()) {
        return t.as_arr()?.iter().find(|t| t.truthy());
    }
    sh.get("tumour")
}

fn on(tm: Option<&Value>) -> Option<&Value> {
    tm.filter(|t| t.truthy() && t.get("on").is_some_and(Value::truthy))
}

fn ints(v: Option<&Value>) -> Vec<usize> {
    v.and_then(Value::as_arr)
        .map(|a| a.iter().filter_map(inum).map(|x| x as usize).collect())
        .unwrap_or_default()
}

/// The joined curve as one path per piece, with its tumours.
pub fn joined_paths(sh: &Value) -> Vec<Vec<P>> {
    let pts = sh.get("pts").and_then(pts_of).unwrap_or_default();
    let gaps = ints(sh.get("gaps"));
    let tms = sh.get("tumours").and_then(Value::as_arr);
    let splits = if tms.is_some() {
        ints(sh.get("splits"))
    } else {
        Vec::new()
    };
    let mut pieces = Vec::new();
    let mut a0 = 0;
    for &g in &gaps {
        pieces.push((a0, g));
        a0 = g + 1;
    }
    pieces.push((a0, anchor_count(pts.len()) - 1));
    let part = |a: usize, b: usize| sample(&pts[3 * a..=3 * b], 240);
    if sh.get("shape").is_some_and(Value::truthy) || sh.get("pattern").is_some_and(Value::truthy) {
        let paths = formed_paths(pieces.iter().map(|&(a, b)| part(a, b)).collect(), sh);
        let tm = on(shown_tumour(sh));
        return paths
            .into_iter()
            .map(|p| match tm {
                Some(t) => tumour_path(&p, t),
                None => p,
            })
            .collect();
    }
    let mut out = Vec::new();
    let mut section = 0;
    for &(p0, p1) in &pieces {
        let Some(tms) = tms else {
            let path = part(p0, p1);
            out.push(match on(sh.get("tumour")) {
                Some(t) => tumour_path(&path, t),
                None => path,
            });
            continue;
        };
        let mut bounds = vec![p0];
        bounds.extend(splits.iter().copied().filter(|&a| p0 < a && a < p1));
        bounds.push(p1);
        let mut path: Vec<P> = Vec::new();
        for w in bounds.windows(2) {
            let mut piece = part(w[0], w[1]);
            if let Some(t) = on(tms.get(section)) {
                piece = tumour_path(&piece, t);
            }
            section += 1;
            if !path.is_empty() && !piece.is_empty() && piece[0] == path[path.len() - 1] {
                path.extend_from_slice(&piece[1..]);
            } else {
                path.extend(piece);
            }
        }
        out.push(path);
    }
    out
}
