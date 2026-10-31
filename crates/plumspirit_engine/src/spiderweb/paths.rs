//! Paths → notes (port of Spiderweb `scripts/notes/paths.py`, MIT © Kanade
//! Tachibana): every line, polyline, freehand stroke, curve and arc becomes one
//! note per key row it crosses. Points are `(tick, pitch)` here (beats already
//! multiplied by PPQ). Numerics follow the Python exactly (see
//! `docs/references/v0_8/SPIDERWEB_NOTES_SPEC.md` §3).

/// Half a pitch row, just inside the row.
pub const EDGE: f64 = 0.5 - 1e-6;
/// The highest key a shape can make (256-key projects; 128-key ones filter).
pub const TOP_KEY: i64 = 255;

pub type P = [f64; 2];
/// `(start, end, pitch)` ticks / key.
pub type Raw = [i64; 3];

#[inline]
pub fn pitch_of(y: f64) -> i64 {
    (y + 0.5).floor() as i64
}

#[inline]
pub fn lerp(a: P, b: P, u: f64) -> P {
    [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]
}

/// The path without points repeating the one before.
pub fn dedupe(path: &[P]) -> Vec<P> {
    let mut out: Vec<P> = Vec::with_capacity(path.len());
    for &p in path {
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    out
}

#[inline]
fn sign(x: f64) -> i8 {
    if x > 0.0 {
        1
    } else if x < 0.0 {
        -1
    } else {
        0
    }
}

/// Segments (i → i + 1) moving the other way than the last segment that moved.
pub fn direction_changes(v: &[f64]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut prev: i8 = 0;
    for i in 0..v.len().saturating_sub(1) {
        let d = sign(v[i + 1] - v[i]);
        if d == 0 {
            continue;
        }
        if prev != 0 && d != prev {
            out.push(i);
        }
        prev = d;
    }
    out
}

fn remap(pts: &mut [P], i: usize, j: usize, move_start: bool, move_end: bool, end_dot: bool) {
    let (ys, ye) = (pts[i][1], pts[j][1]);
    let (ps, pe) = (pitch_of(ys), pitch_of(ye));
    if ps == pe {
        return;
    }
    let d = if pe > ps { 1.0 } else { -1.0 };
    let (psf, pef) = (ps as f64, pe as f64);
    let ns = if move_start { psf - d * EDGE } else { ys };
    let ne = if move_end {
        if end_dot {
            pef - d * EDGE
        } else {
            pef + d * EDGE
        }
    } else {
        ye
    };
    let k = (ne - ns) / (ye - ys);
    for p in &mut pts[i..=j] {
        p[1] = ns + (p[1] - ys) * k;
    }
}

/// Gives the first and last key of the path an equal share of time.
pub fn stretch_ends(path: &[P], end_dot: bool) -> Vec<P> {
    let mut pts = path.to_vec();
    let n = pts.len();
    if n < 2 {
        return pts;
    }
    let ys: Vec<f64> = pts.iter().map(|p| p[1]).collect();
    let mut turns = vec![0usize];
    turns.extend(direction_changes(&ys));
    turns.push(n - 1);
    if turns.len() == 2 {
        remap(&mut pts, 0, n - 1, true, true, end_dot);
    } else {
        remap(&mut pts, 0, turns[1], true, false, false);
        remap(
            &mut pts,
            turns[turns.len() - 2],
            n - 1,
            false,
            true,
            end_dot,
        );
    }
    pts
}

/// The same key starting on the same tick twice: the longest stays (in the
/// order the first of them came). Notes ending before tick 0 are dropped.
pub fn keep_longest(raw: &[Raw]) -> Vec<Raw> {
    let raw: Vec<Raw> = raw.iter().copied().filter(|r| r[1] >= 0).collect();
    if raw.len() < 2 {
        return raw;
    }
    let mut order: Vec<usize> = (0..raw.len()).collect();
    order.sort_by_key(|&i| (raw[i][0], raw[i][2], i));
    // (first original index, start, max end, pitch)
    let mut groups: Vec<(usize, Raw)> = Vec::new();
    for &i in &order {
        let r = raw[i];
        match groups.last_mut() {
            Some((_, g)) if g[0] == r[0] && g[2] == r[2] => g[1] = g[1].max(r[1]),
            _ => groups.push((i, r)),
        }
    }
    groups.sort_by_key(|g| g.0);
    groups.into_iter().map(|g| g.1).collect()
}

/// A closed loop restarted at its leftmost (then lowest) point.
pub fn loop_from_left(path: &[P]) -> Vec<P> {
    let p = &path[..path.len() - 1];
    let mut best = 0;
    for (i, q) in p.iter().enumerate() {
        let b = p[best];
        if q[0] < b[0] || (q[0] == b[0] && q[1] < b[1]) {
            best = i;
        }
    }
    let mut out = p[best..].to_vec();
    out.extend_from_slice(&p[..=best]);
    out
}

/// Does the path reach its last point moving forward in time?
pub fn ends_forward(path: &[P]) -> bool {
    for i in (0..path.len().saturating_sub(1)).rev() {
        let dt = path[i + 1][0] - path[i][0];
        if dt.abs() > 1e-12 {
            return dt > 0.0;
        }
    }
    true
}

/// A line / curve / arc path (ticks) → notes.
pub fn path_notes(path: &[P], end_dot: bool) -> Vec<Raw> {
    if path.is_empty() {
        return Vec::new();
    }
    let (path, end_dot) = if path.len() > 3 && path[0] == path[path.len() - 1] {
        (loop_from_left(path), false)
    } else {
        let e = end_dot && ends_forward(path);
        (stretch_ends(path, e), e)
    };
    keep_longest(&line_notes(&path, end_dot))
}

/// Points → notes before `keep_longest`: split where the path turns back in
/// time; vertical stretches are parts of their own.
pub fn line_notes(pts: &[P], tail: bool) -> Vec<Raw> {
    let n = pts.len();
    if n == 0 {
        return Vec::new();
    }
    let xs: Vec<f64> = pts.iter().map(|p| p[0]).collect();
    let cuts = direction_changes(&xs);
    let mut lo = vec![0usize];
    lo.extend(&cuts);
    let mut hi = cuts.clone();
    hi.push(n - 1);
    let mut q: Vec<P> = Vec::with_capacity(n + cuts.len());
    let mut piece_end: Vec<usize> = Vec::with_capacity(lo.len());
    let mut piece_start: Vec<usize> = Vec::with_capacity(lo.len());
    for (&a, &b) in lo.iter().zip(&hi) {
        piece_start.push(q.len());
        if pts[b][0] < pts[a][0] {
            q.extend(pts[a..=b].iter().rev());
        } else {
            q.extend_from_slice(&pts[a..=b]);
        }
        piece_end.push(q.len() - 1);
    }
    let m = q.len();
    // same[i]: q[i] → q[i + 1] is a segment (not the jump to the next piece).
    let mut same = vec![true; m.saturating_sub(1)];
    for &e in &piece_end[..piece_end.len() - 1] {
        same[e] = false;
    }
    let up: Vec<bool> = (0..m.saturating_sub(1))
        .map(|i| (q[i + 1][0] - q[i][0]).abs() < 1e-9)
        .collect();
    let mut split = Vec::new();
    for i in 0..m.saturating_sub(2) {
        if same[i + 1] && same[i] && up[i + 1] != up[i] {
            split.push(i + 1);
        }
    }
    let mut first: Vec<usize> = piece_start.iter().chain(&split).copied().collect();
    first.sort_unstable();
    let mut last: Vec<usize> = split.iter().chain(&piece_end).copied().collect();
    last.sort_unstable();
    let tails: Vec<bool> = last
        .iter()
        .map(|l| tail && piece_end.contains(l) && q[*l] == pts[n - 1])
        .collect();
    let mut r: Vec<P> = Vec::new();
    let mut offs = Vec::with_capacity(first.len());
    for (&f, &l) in first.iter().zip(&last) {
        offs.push(r.len());
        r.extend_from_slice(&q[f..=l]);
    }
    parts_notes(&r, &offs, &tails).0
}

/// Parts running left to right → notes, one per key crossed, and how many
/// notes each part made. Part k = `r[first[k]] .. r[first[k+1] - 1]`.
pub fn parts_notes(r: &[P], first: &[usize], tails: &[bool]) -> (Vec<Raw>, Vec<usize>) {
    let k_parts = first.len();
    if k_parts == 0 || r.is_empty() {
        return (Vec::new(), vec![0; k_parts]);
    }
    let p: Vec<i64> = r.iter().map(|q| pitch_of(q[1])).collect();
    let last: Vec<usize> = (0..k_parts)
        .map(|k| {
            if k + 1 < k_parts {
                first[k + 1] - 1
            } else {
                r.len() - 1
            }
        })
        .collect();
    // Entries per part: (time, pitch).
    let mut et: Vec<f64> = Vec::new();
    let mut ep: Vec<i64> = Vec::new();
    let mut per = vec![0usize; k_parts];
    let mut part_of: Vec<usize> = Vec::new();
    for k in 0..k_parts {
        let f = first[k];
        et.push(r[f][0]);
        ep.push(p[f]);
        part_of.push(k);
        per[k] = 1;
        for j in f..last[k] {
            let (pa, pb) = (p[j], p[j + 1]);
            if pa == pb {
                continue;
            }
            let st = if pb > pa { 1i64 } else { -1 };
            let c = (pb - pa).abs();
            let (ta, tb, ya, yb) = (r[j][0], r[j + 1][0], r[j][1], r[j + 1][1]);
            for s in 1..=c {
                let qq = pa + st * s;
                let yy = qq as f64 - 0.5 * st as f64;
                let tt = ta + (tb - ta) * (yy - ya) / (yb - ya);
                et.push(tt);
                ep.push(qq);
                part_of.push(k);
                per[k] += 1;
            }
        }
    }
    let m = et.len();
    let starts: Vec<i64> = et.iter().map(|t| (t + 0.5).floor() as i64).collect();
    let end_tick: Vec<i64> = last
        .iter()
        .map(|&l| (r[l][0] + 0.5).floor() as i64)
        .collect();
    let mut off = vec![0usize; k_parts];
    for k in 1..k_parts {
        off[k] = off[k - 1] + per[k - 1];
    }
    let e_last: Vec<usize> = (0..k_parts).map(|k| off[k] + per[k] - 1).collect();
    // nxt_at[i]: first index >= i where a later start follows in the same part.
    let mut nxt_at = vec![m; m];
    let mut acc = m;
    for i in (0..m).rev() {
        let later = i + 1 < m && part_of[i + 1] == part_of[i] && starts[i + 1] > starts[i];
        if later {
            acc = i;
        }
        nxt_at[i] = acc;
    }
    let mut ends = vec![0i64; m];
    for i in 0..m {
        let k = part_of[i];
        let nxt = if nxt_at[i] < e_last[k] {
            starts[(nxt_at[i] + 1).min(m - 1)]
        } else {
            end_tick[k]
        };
        ends[i] = nxt.max(starts[i] + 1);
    }
    for i in 0..m.saturating_sub(1) {
        if part_of[i + 1] == part_of[i] && starts[i + 1] == starts[i] {
            ends[i] = starts[i] + 1;
        }
    }
    for k in 0..k_parts {
        if tails.get(k).copied().unwrap_or(false) && per[k] >= 2 {
            let tl = e_last[k];
            if starts[tl] == end_tick[k] {
                ends[tl] = starts[tl] + (ends[tl - 1] - starts[tl - 1]).max(1);
            }
        }
    }
    let notes = (0..m).map(|i| [starts[i], ends[i], ep[i]]).collect();
    (notes, per)
}

/// Polyline with "starts exactly on the last point": every segment like its
/// own line, so every dot's note starts on that dot.
pub fn dot_segment_notes(path: &[P]) -> Vec<Raw> {
    let n = path.len().saturating_sub(1);
    if n == 0 {
        return Vec::new();
    }
    let mut pts: Vec<P> = Vec::with_capacity(2 * n);
    for i in 0..n {
        let (mut a, mut b) = (path[i], path[i + 1]);
        if a[0] > b[0] || (a[0] == b[0] && a[1] > b[1]) {
            std::mem::swap(&mut a, &mut b);
        }
        let (ys, ye) = (a[1], b[1]);
        let (ps, pe) = ((ys + 0.5).floor(), (ye + 0.5).floor());
        let d = if pe > ps { 1.0 } else { -1.0 };
        let (ns, ne) = (ps - d * EDGE, pe - d * EDGE);
        if ps != pe {
            let k = (ne - ns) / (ye - ys);
            let y0 = ns; // ns + (ys − ys)·k with k finite
            let y1 = ns + (ye - ys) * k;
            pts.push([a[0], y0]);
            pts.push([b[0], y1]);
        } else {
            pts.push([a[0], ys]);
            pts.push([b[0], ye]);
        }
    }
    let first: Vec<usize> = (0..n).map(|i| 2 * i).collect();
    let (raw, per) = parts_notes(&pts, &first, &vec![true; n]);
    // Tails: the last note of each segment that has two or more.
    let mut tail_rows: Vec<Raw> = Vec::new();
    let mut acc = 0usize;
    for &c in &per {
        acc += c;
        if c >= 2 {
            tail_rows.push(raw[acc - 1]);
        }
    }
    let in_tails: Vec<bool> = raw.iter().map(|r| tail_rows.contains(r)).collect();
    let mut keyed: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
    for (r, t) in raw.iter().zip(&in_tails) {
        if !t {
            keyed.insert((r[0], r[2]));
        }
    }
    raw.iter()
        .zip(&in_tails)
        .filter(|(r, t)| !(**t && keyed.contains(&(r[0], r[2]))))
        .map(|(r, _)| *r)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_over_five_keys_gives_equal_shares() {
        // The Python: [[0,384,60],[384,768,61],[768,1152,62],[1152,1536,63],[1536,1920,64]].
        let n = path_notes(&[[0.0, 60.0], [1920.0, 64.0]], false);
        assert_eq!(
            n,
            vec![
                [0, 384, 60],
                [384, 768, 61],
                [768, 1152, 62],
                [1152, 1536, 63],
                [1536, 1920, 64]
            ]
        );
    }

    #[test]
    fn direction_changes_skip_flat_segments() {
        assert_eq!(
            direction_changes(&[0.0, 1.0, 1.0, 2.0, 1.0, 1.0, 3.0]),
            vec![3, 5]
        );
        assert!(direction_changes(&[1.0, 1.0]).is_empty());
    }
}
