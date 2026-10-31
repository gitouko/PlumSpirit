//! Areas of a custom shape (areas.py): the pieces its lines cut the
//! drawing into (outline-and-fill loops and fill lines). Each area can get a
//! colour of its own or be left empty: `sh["areas"] = [[u, v, colour]]`.
//! Areas are found on a grid of `MAP_CELLS` × `MAP_CELLS` cells.

use super::np::{dist, hypot};
use super::paths::P;
use super::{fnum, inum};
use crate::json::Value;

pub const MAP_CELLS: usize = 1024;
pub const COLOURS: i64 = 15;
/// Cells a fill line's loose ends reach further.
pub const REACH: f64 = 3.0;

/// NumPy's float → int64 cast on x86 (NaN / out of range → `i64::MIN`).
#[inline]
pub fn np_i64(x: f64) -> i64 {
    if x.is_nan() || !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&x) {
        i64::MIN
    } else {
        x as i64
    }
}

/// `np.minimum` (NaN wins).
#[inline]
pub fn npmin(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if b < a {
        b
    } else {
        a
    }
}

/// `np.maximum` (NaN wins).
#[inline]
pub fn npmax(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if b > a {
        b
    } else {
        a
    }
}

/// A saved list of areas → `[[u, v, colour]]` (bad ones left out).
pub fn clean_areas(areas: Option<&Value>) -> Vec<(f64, f64, i64)> {
    let mut out = Vec::new();
    let Some(list) = areas.and_then(Value::as_arr) else {
        return out;
    };
    for a in list {
        let Some(t) = a.as_arr() else { continue };
        if t.len() < 3 {
            continue;
        }
        let (Some(u), Some(v), Some(c)) = (fnum(&t[0]), fnum(&t[1]), inum(&t[2])) else {
            continue;
        };
        if (0..=COLOURS).contains(&c) && u.is_finite() && v.is_finite() {
            out.push((u, v, c));
        }
    }
    out
}

pub fn areas_value(a: &[(f64, f64, i64)]) -> Value {
    Value::Arr(
        a.iter()
            .map(|(u, v, c)| {
                Value::Arr(vec![Value::Num(*u), Value::Num(*v), Value::Num(*c as f64)])
            })
            .collect(),
    )
}

/// Which area each spot of the drawing is in.
pub struct AreaMap {
    pub k: [f64; 2],
    pub lo: [f64; 2],
    pub w: usize,
    pub h: usize,
    labels: Vec<i32>,
    pub count: usize,
    pub spots: Vec<P>,
}

impl AreaMap {
    pub fn new(loops: &[Vec<P>], cuts: &[Vec<P>]) -> AreaMap {
        let paths: Vec<&Vec<P>> = loops.iter().filter(|p| !p.is_empty()).collect();
        let mut cut_paths: Vec<Vec<P>> = cuts.iter().filter(|p| !p.is_empty()).cloned().collect();
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        let mut any = false;
        for p in paths
            .iter()
            .map(|p| p.as_slice())
            .chain(cut_paths.iter().map(Vec::as_slice))
        {
            for q in p {
                any = true;
                for d in 0..2 {
                    lo[d] = npmin(lo[d], q[d]);
                    hi[d] = npmax(hi[d], q[d]);
                }
            }
        }
        if !any {
            lo = [0.0; 2];
            hi = [0.0; 2];
        }
        let n = MAP_CELLS as f64;
        let k = [
            n / npmax(hi[0] - lo[0], 1e-9),
            n / npmax(hi[1] - lo[1], 1e-9),
        ];
        let lo = [lo[0] - (REACH + 2.0) / k[0], lo[1] - (REACH + 2.0) / k[1]];
        let w = MAP_CELLS + 2 * (REACH as usize + 2) + 1;
        let h = w;
        for p in &mut cut_paths {
            if p.len() >= 2 && dist(p[0], p[p.len() - 1]) > 1e-9 {
                let c: Vec<P> = p
                    .iter()
                    .map(|q| [(q[0] - lo[0]) * k[0], (q[1] - lo[1]) * k[1]])
                    .collect();
                let mut np_ = c.clone();
                let last = c.len() - 1;
                for (at, nxt) in [(0, 1), (last, last - 1)] {
                    let d = [c[at][0] - c[nxt][0], c[at][1] - c[nxt][1]];
                    let ln = hypot(d[0], d[1]);
                    if ln > 1e-12 {
                        np_[at] = [c[at][0] + d[0] / ln * REACH, c[at][1] + d[1] / ln * REACH];
                    }
                }
                *p = np_
                    .iter()
                    .map(|q| [q[0] / k[0] + lo[0], q[1] / k[1] + lo[1]])
                    .collect();
            }
        }
        let mut wall = vec![false; w * h];
        let all: Vec<&[P]> = paths
            .iter()
            .map(|p| p.as_slice())
            .chain(cut_paths.iter().map(Vec::as_slice))
            .collect();
        for p in &all {
            if p.len() < 2 {
                continue;
            }
            for s in p.windows(2) {
                let a = [(s[0][0] - lo[0]) * k[0], (s[0][1] - lo[1]) * k[1]];
                let b = [(s[1][0] - lo[0]) * k[0], (s[1][1] - lo[1]) * k[1]];
                let span = npmax((b[0] - a[0]).abs(), (b[1] - a[1]).abs());
                let m = np_i64((span * 2.0).ceil()).wrapping_add(1);
                let m = m.max(0) as usize;
                let div = m.saturating_sub(1).max(1) as f64;
                for j in 0..m {
                    let t = j as f64 / div;
                    let x = a[0] + (b[0] - a[0]) * t;
                    let y = a[1] + (b[1] - a[1]) * t;
                    let cx = np_i64(x.floor()).clamp(0, w as i64 - 1) as usize;
                    let cy = np_i64(y.floor()).clamp(0, h as i64 - 1) as usize;
                    wall[cy * w + cx] = true;
                }
            }
        }
        for p in &all {
            if p.len() == 1 {
                let cx = np_i64(((p[0][0] - lo[0]) * k[0]).floor()).clamp(0, w as i64 - 1) as usize;
                let cy = np_i64(((p[0][1] - lo[1]) * k[1]).floor()).clamp(0, h as i64 - 1) as usize;
                wall[cy * w + cx] = true;
            }
        }
        let mut m = AreaMap {
            k,
            lo,
            w,
            h,
            labels: Vec::new(),
            count: 0,
            spots: Vec::new(),
        };
        m.label(&wall);
        m
    }

    /// Every free cell's area number (walls −1), the count, a spot in each
    /// (areas numbered in the order of their first run, row by row).
    fn label(&mut self, wall: &[bool]) {
        let (w, h) = (self.w, self.h);
        // runs of free cells: (row, start, end)
        let mut runs: Vec<(usize, usize, usize)> = Vec::new();
        let mut row_first = vec![0usize; h + 1];
        for y in 0..h {
            row_first[y] = runs.len();
            let r = &wall[y * w..(y + 1) * w];
            let mut x = 0;
            while x < w {
                if r[x] {
                    x += 1;
                    continue;
                }
                let s = x;
                while x < w && !r[x] {
                    x += 1;
                }
                runs.push((y, s, x));
            }
        }
        row_first[h] = runs.len();
        self.labels = vec![-1; w * h];
        if runs.is_empty() {
            return;
        }
        let mut parent: Vec<usize> = (0..runs.len()).collect();
        fn find(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        for y in 0..h.saturating_sub(1) {
            let (a0, a1) = (row_first[y], row_first[y + 1]);
            let (b0, b1) = (row_first[y + 1], row_first[y + 2]);
            let mut j = b0;
            for i in a0..a1 {
                let (_, s, e) = runs[i];
                while j < b1 && runs[j].2 <= s {
                    j += 1;
                }
                let mut q = j;
                while q < b1 && runs[q].1 < e {
                    let (ri, rq) = (find(&mut parent, i), find(&mut parent, q));
                    if ri != rq {
                        let (lo, hi) = if ri < rq { (ri, rq) } else { (rq, ri) };
                        parent[hi] = lo;
                    }
                    q += 1;
                }
            }
        }
        let mut area = vec![0i32; runs.len()];
        let mut id_of_root = vec![-1i32; runs.len()];
        let mut count = 0i32;
        for i in 0..runs.len() {
            let r = find(&mut parent, i);
            if id_of_root[r] < 0 {
                id_of_root[r] = count;
                count += 1;
                let (y, s, e) = runs[i];
                self.spots.push([
                    ((s + e) as f64 / 2.0) / self.k[0] + self.lo[0],
                    (y as f64 + 0.5) / self.k[1] + self.lo[1],
                ]);
            }
            area[i] = id_of_root[r];
        }
        for (i, &(y, s, e)) in runs.iter().enumerate() {
            for x in s..e {
                self.labels[y * w + x] = area[i];
            }
        }
        self.count = count as usize;
    }

    #[inline]
    fn label_at(&self, cx: i64, cy: i64) -> Option<i32> {
        if cx >= 0 && (cx as usize) < self.w && cy >= 0 && (cy as usize) < self.h {
            Some(self.labels[cy as usize * self.w + cx as usize])
        } else {
            None
        }
    }

    /// The area number at a spot, −1 on a wall, `off` off the map.
    pub fn cell(&self, u: f64, v: f64, off: i32) -> i32 {
        let cx = np_i64(((u - self.lo[0]) * self.k[0]).floor());
        let cy = np_i64(((v - self.lo[1]) * self.k[1]).floor());
        self.label_at(cx, cy).unwrap_or(off)
    }

    /// The area number at a spot; on a wall the nearest free cell (−1: none).
    pub fn at(&self, u: f64, v: f64) -> i32 {
        let fx = (u - self.lo[0]) * self.k[0];
        let fy = (v - self.lo[1]) * self.k[1];
        let cx = np_i64(fx.floor());
        let cy = np_i64(fy.floor());
        let mut out = self.label_at(cx, cy).unwrap_or(-1);
        if out >= 0 {
            return out;
        }
        // (np.where(f - c < 0.5, -1, 1): NaN compares false → 1)
        let sx: i64 = if fx - (cx as f64) < 0.5 { -1 } else { 1 };
        let sy: i64 = if fy - (cy as f64) < 0.5 { -1 } else { 1 };
        for (dx, dy) in [
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (2, 0),
            (-2, 0),
            (0, 2),
            (0, -2),
            (1, 1),
            (-1, 1),
            (1, -1),
            (-1, -1),
        ] {
            let x = cx.wrapping_add(dx * sx);
            let y = cy.wrapping_add(dy * sy);
            out = self.label_at(x, y).unwrap_or(-1);
            if out >= 0 {
                break;
            }
        }
        out
    }

    /// The area around the drawing (touching the map's edge).
    pub fn outside(&self) -> i32 {
        self.labels[0]
    }
}

/// Which spots are inside the closed loops (even-odd; union: any of them).
pub fn inside_loops(spots: &[P], loops: &[Vec<P>], union: bool) -> Vec<bool> {
    let mut inside = vec![false; spots.len()];
    let mut count = vec![0i64; spots.len()];
    for p in loops {
        if p.len() < 3 {
            continue;
        }
        for (s, (ins, cnt)) in spots.iter().zip(inside.iter_mut().zip(count.iter_mut())) {
            let (u, v) = (s[0], s[1]);
            let mut hit = 0i64;
            for e in p.windows(2) {
                let (a, b, c, d) = (e[0][0], e[0][1], e[1][0], e[1][1]);
                let cross = (b > v) != (d > v);
                let x = a + (c - a) * (v - b) / if d == b { 1.0 } else { d - b };
                if cross && x > u {
                    hit += 1;
                }
            }
            if union {
                *ins |= hit % 2 == 1;
            } else {
                *cnt += hit;
            }
        }
    }
    if union {
        inside
    } else {
        count.iter().map(|c| c % 2 == 1).collect()
    }
}
