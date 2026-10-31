//! Spatial structures for geometry and masking tools (contract §144): a note
//! set as **row intervals** — for each pitch row of the project's keyboard
//! (128 rows, 256 in a 256-Key Mode project), the sorted,
//! disjoint tick intervals the notes cover. Building is `O(m log m)`; clipping a
//! note is a binary search in its row (`O(log k + pieces)`), so masking `n`
//! notes against `m` is `O((n + m) log m)`, never quadratic.

use super::TNote;
use crate::model::project::KeyRange;

/// Sorted, disjoint `[a, b)` intervals per pitch row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowMask {
    rows: Vec<Vec<(u64, u64)>>,
}

fn normalize(v: &mut Vec<(u64, u64)>) {
    v.retain(|r| r.1 > r.0);
    v.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    for &(a, b) in v.iter() {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    *v = out;
}

impl RowMask {
    /// No intervals on any row of a `keys` keyboard.
    pub fn empty(keys: KeyRange) -> RowMask {
        RowMask {
            rows: vec![Vec::new(); keys.keys()],
        }
    }

    /// The same rows, all empty.
    fn empty_like(&self) -> RowMask {
        RowMask {
            rows: vec![Vec::new(); self.rows.len()],
        }
    }

    /// The union of the notes' rectangles (notes outside the keyboard `keys`
    /// cover nothing).
    pub fn from_notes<'a>(keys: KeyRange, notes: impl IntoIterator<Item = &'a TNote>) -> RowMask {
        let mut m = RowMask::empty(keys);
        for n in notes {
            if let Some(r) = m.rows.get_mut(usize::from(n.pitch)) {
                r.push((n.start, n.end()));
            }
        }
        for r in &mut m.rows {
            normalize(r);
        }
        m
    }

    /// The given rows (row `i` = pitch `i`), each normalized (sorted,
    /// overlapping or touching intervals merged, empty ones dropped).
    pub fn from_rows(mut rows: Vec<Vec<(u64, u64)>>) -> RowMask {
        for r in &mut rows {
            normalize(r);
        }
        RowMask { rows }
    }

    /// Rows of the mask (the keyboard's keys, or 1 for a pitch-free mask).
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The earliest start and the latest end over all rows (`None` when
    /// empty).
    pub fn extent(&self) -> Option<(u64, u64)> {
        let a = self
            .rows
            .iter()
            .filter_map(|r| r.first().map(|x| x.0))
            .min()?;
        let b = self
            .rows
            .iter()
            .filter_map(|r| r.last().map(|x| x.1))
            .max()?;
        Some((a, b))
    }

    /// Number of intervals of row `pitch` that intersect `[s, e)` — the
    /// pieces [`RowMask::clip`] gives (two binary searches).
    pub fn overlapping(&self, pitch: u8, s: u64, e: u64) -> usize {
        if s >= e {
            return 0;
        }
        let r = self.row(pitch);
        let lo = r.partition_point(|x| x.1 <= s);
        let hi = r.partition_point(|x| x.0 < e);
        hi.saturating_sub(lo)
    }

    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(Vec::is_empty)
    }

    /// The intervals of row `pitch` (empty outside the keyboard).
    pub fn row(&self, pitch: u8) -> &[(u64, u64)] {
        self.rows.get(usize::from(pitch)).map_or(&[], Vec::as_slice)
    }

    pub fn intervals(&self) -> usize {
        self.rows.iter().map(Vec::len).sum()
    }

    pub fn union(&self, o: &RowMask) -> RowMask {
        let mut m = self.clone();
        for (r, x) in m.rows.iter_mut().zip(&o.rows) {
            r.extend_from_slice(x);
            normalize(r);
        }
        m
    }

    pub fn intersect(&self, o: &RowMask) -> RowMask {
        let mut m = self.empty_like();
        for ((out, a), b) in m.rows.iter_mut().zip(&self.rows).zip(&o.rows) {
            let (mut i, mut j) = (0, 0);
            while i < a.len() && j < b.len() {
                let lo = a[i].0.max(b[j].0);
                let hi = a[i].1.min(b[j].1);
                if lo < hi {
                    out.push((lo, hi));
                }
                if a[i].1 < b[j].1 {
                    i += 1;
                } else {
                    j += 1;
                }
            }
        }
        m
    }

    /// The complement inside `[t0, t1)` on every row.
    pub fn invert(&self, t0: u64, t1: u64) -> RowMask {
        let mut m = self.empty_like();
        for (out, r) in m.rows.iter_mut().zip(&self.rows) {
            let mut at = t0;
            for &(a, b) in r {
                if b <= t0 || a >= t1 {
                    continue;
                }
                if a > at {
                    out.push((at, a));
                }
                at = at.max(b);
            }
            if at < t1 {
                out.push((at, t1));
            }
        }
        m
    }

    pub fn subtract(&self, o: &RowMask, t0: u64, t1: u64) -> RowMask {
        self.intersect(&o.invert(t0, t1))
    }

    /// Grows every interval by `ticks` on both sides and every row into its
    /// `rows` neighbours (dilation).
    pub fn expand(&self, ticks: u64, rows: u8) -> RowMask {
        let mut m = self.empty_like();
        let top = self.rows.len().saturating_sub(1);
        for (p, r) in self.rows.iter().enumerate() {
            let lo = p.saturating_sub(usize::from(rows));
            let hi = (p + usize::from(rows)).min(top);
            for q in lo..=hi {
                m.rows[q].extend(
                    r.iter()
                        .map(|&(a, b)| (a.saturating_sub(ticks), b.saturating_add(ticks))),
                );
            }
        }
        for r in &mut m.rows {
            normalize(r);
        }
        m
    }

    /// Shrinks the mask (erosion: the complement is dilated) inside
    /// `[t0, t1)`.
    pub fn contract(&self, ticks: u64, rows: u8, t0: u64, t1: u64) -> RowMask {
        // Work shifted right so the outside before tick 0 exists too.
        let off = ticks.saturating_add(1);
        let shifted = self.shift(off);
        let outer_end = t1.saturating_add(2 * off);
        let comp = shifted.invert(0, outer_end);
        // Rows outside the keyboard are "outside the mask" too.
        let mut c = comp.expand(ticks, rows);
        let keys = c.rows.len();
        for p in 0..usize::from(rows).min(keys) {
            c.rows[p] = vec![(0, outer_end)];
            c.rows[keys - 1 - p] = vec![(0, outer_end)];
        }
        c.invert(t0.saturating_add(off), t1.saturating_add(off))
            .shift_back(off)
    }

    /// The mask moved `dt` ticks later.
    pub fn shift(&self, dt: u64) -> RowMask {
        RowMask {
            rows: self
                .rows
                .iter()
                .map(|r| {
                    r.iter()
                        .map(|&(a, b)| (a.saturating_add(dt), b.saturating_add(dt)))
                        .collect()
                })
                .collect(),
        }
    }

    /// The mask moved `dt` ticks earlier (clipped at 0).
    pub fn shift_back(&self, dt: u64) -> RowMask {
        let mut m = RowMask {
            rows: self
                .rows
                .iter()
                .map(|r| {
                    r.iter()
                        .map(|&(a, b)| (a.saturating_sub(dt), b.saturating_sub(dt)))
                        .collect()
                })
                .collect(),
        };
        for r in &mut m.rows {
            normalize(r);
        }
        m
    }

    /// Calls `f(a, b)` for each piece of `[s, e)` on row `pitch` inside the mask.
    pub fn clip(&self, pitch: u8, s: u64, e: u64, mut f: impl FnMut(u64, u64)) {
        let r = self.row(pitch);
        let i = r.partition_point(|x| x.1 <= s);
        for &(a, b) in &r[i..] {
            if a >= e {
                break;
            }
            let lo = a.max(s);
            let hi = b.min(e);
            if lo < hi {
                f(lo, hi);
            }
        }
    }

    /// Distance from tick `t` on row `pitch` to the nearest mask edge in time
    /// (`u64::MAX` for an empty row).
    pub fn edge_distance(&self, pitch: u8, t: u64) -> u64 {
        let r = self.row(pitch);
        let i = r.partition_point(|x| x.1 <= t);
        let mut d = u64::MAX;
        for k in [i.wrapping_sub(1), i, i + 1] {
            if let Some(&(a, b)) = r.get(k) {
                d = d.min(t.abs_diff(a)).min(t.abs_diff(b));
            }
        }
        d
    }
}

// ============================================================ Shape morphology

/// A structuring element for [`RowMask::dilate`] / [`RowMask::erode`]: `kx`
/// ticks along time and `ky` rows along pitch. A **box** reaches `kx` ticks
/// on every row within `ky` (square corners, 8-connected); a **diamond**
/// reaches fewer ticks the farther the row ([`Shape::reach`]: cut corners,
/// 4-connected for one cell).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub kx: u64,
    pub ky: u32,
    pub diamond: bool,
}

impl Shape {
    /// Time reach (ticks) on the row `j` rows away (`j ≤ ky`): the box's `kx`;
    /// the diamond's `kx · (ky − j) / ky` (rounded down; `kx` when `ky` is 0)
    /// — the L1 ball of radii `kx` and `ky`, whose outermost rows are the
    /// intervals themselves.
    pub fn reach(&self, j: u32) -> u64 {
        if !self.diamond || self.ky == 0 {
            return self.kx;
        }
        let (kx, ky, j) = (
            u128::from(self.kx),
            u128::from(self.ky),
            u128::from(j.min(self.ky)),
        );
        (kx * (ky - j) / ky) as u64
    }

    pub fn is_empty(&self) -> bool {
        self.kx == 0 && self.ky == 0
    }
}

impl RowMask {
    /// Dilation by `shape` (Minkowski sum): every interval grows by the
    /// shape's reach on each row within `ky` (clipped to the keyboard and at
    /// tick 0). `O(intervals · (2 ky + 1))` plus the per-row sorts — never
    /// note against note.
    pub fn dilate(&self, shape: Shape) -> RowMask {
        if shape.is_empty() {
            return self.clone();
        }
        let mut m = self.empty_like();
        let top = self.rows.len() as i64 - 1;
        let ky = i64::from(shape.ky);
        for (p, r) in self.rows.iter().enumerate() {
            if r.is_empty() {
                continue;
            }
            for j in -ky..=ky {
                let q = p as i64 + j;
                if q < 0 || q > top {
                    continue;
                }
                let k = shape.reach(j.unsigned_abs() as u32);
                m.rows[q as usize].extend(
                    r.iter()
                        .map(|&(a, b)| (a.saturating_sub(k), b.saturating_add(k))),
                );
            }
        }
        for r in &mut m.rows {
            normalize(r);
        }
        m
    }

    /// Erosion by `shape` inside `[t0, t1)`: the points whose whole shape
    /// lies in the mask — the complement of the complement's dilation. Rows
    /// outside the keyboard and ticks outside `[t0, t1)` count as outside
    /// the mask (an edge at the keyboard's end is an edge).
    pub fn erode(&self, shape: Shape, t0: u64, t1: u64) -> RowMask {
        if shape.is_empty() {
            return self.clone();
        }
        // Work shifted right so the outside before t0 exists too.
        let off = shape.kx.saturating_add(1);
        let lo = t0.saturating_add(off);
        let hi = t1.saturating_add(off);
        let end = hi.saturating_add(off);
        let comp = self.shift(off).invert(lo, hi);
        // The outside before lo and after hi on every row.
        let mut c = RowMask {
            rows: comp
                .rows
                .into_iter()
                .map(|mut r| {
                    r.push((0, lo));
                    r.push((hi, end));
                    normalize(&mut r);
                    r
                })
                .collect(),
        }
        .dilate(shape);
        // Rows within ky of the keyboard's ends touch the outside rows.
        let keys = c.rows.len();
        for p in 0..(shape.ky as usize).min(keys) {
            c.rows[p] = vec![(0, end)];
            c.rows[keys - 1 - p] = vec![(0, end)];
        }
        c.invert(lo, hi).shift_back(off)
    }

    /// Every row clipped to `[lo, hi)`.
    pub fn clip_time(&self, lo: u64, hi: u64) -> RowMask {
        let mut m = self.clone();
        for r in &mut m.rows {
            r.retain(|x| x.1 > lo && x.0 < hi);
            for x in r.iter_mut() {
                *x = (x.0.max(lo), x.1.min(hi));
            }
        }
        m
    }

    /// Rows as `(pitch, intervals)`, empty rows skipped.
    pub fn rows(&self) -> impl Iterator<Item = (usize, &[(u64, u64)])> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.is_empty())
            .map(|(p, r)| (p, r.as_slice()))
    }
}

// ===================================================================== Grid

/// Most cells of one [`Grid`] (4 M cells: 16 MB of representatives). Larger
/// selections coarsen the cell width automatically.
pub const GRID_MAX_CELLS: usize = 4 << 20;

/// A bounded occupancy grid over note art (contract §144): columns of `cell`
/// ticks from `t0`, rows of `row_h` semitones covering the project's keys
/// (0–127, or 0–255 in 256-Key Mode). Each cell
/// holds the index + 1 of a representative note (the earliest-starting note
/// covering it; 0 = empty) and the number of notes covering it.
///
/// Building is `O(n + cells)` (each cell is written once per row sweep, counts
/// come from difference arrays), so grid tools (contour, skeleton, cellular
/// growth, density) never compare notes pairwise.
#[derive(Clone, Debug)]
pub struct Grid {
    pub t0: u64,
    pub cell: u64,
    pub row_h: u8,
    /// The keyboard's highest pitch (127, or 255 in 256-Key Mode).
    pub top: u8,
    /// The keyboard row of grid row 0: 0 for the whole keyboard, the band's
    /// lowest row for [`Grid::build_band`] (V09-S50).
    pub row0: usize,
    pub w: usize,
    pub h: usize,
    pub rep: Vec<u32>,
    pub count: Vec<u32>,
}

impl Grid {
    /// The grid of `notes[i]` for `i` in `idx` (notes sorted by start) on the
    /// keyboard `keys`. The box is `[t0, t1)` widened by `pad` cells on both
    /// sides; `cell` is the wished width (coarsened to fit [`GRID_MAX_CELLS`]).
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        keys: KeyRange,
        notes: &[TNote],
        idx: &[usize],
        t0: u64,
        t1: u64,
        cell: u64,
        row_h: u8,
        pad: usize,
    ) -> Grid {
        let row_h = row_h.max(1);
        let h = keys.keys().div_ceil(usize::from(row_h));
        Grid::build_rows(
            keys,
            notes,
            idx,
            (t0, t1),
            cell,
            row_h,
            pad,
            (0, h),
            GRID_MAX_CELLS,
        )
    }

    /// [`Grid::build`] over the pitch band `lo ..= hi` only (one semitone a
    /// row; V09-S50): a tool whose work stays near the art spends its cell
    /// budget (`max_cells`) on time instead of on empty keyboard rows. Every
    /// note of `idx` must lie inside the band.
    #[allow(clippy::too_many_arguments)]
    pub fn build_band(
        keys: KeyRange,
        notes: &[TNote],
        idx: &[usize],
        t0: u64,
        t1: u64,
        cell: u64,
        pad: usize,
        band: (u8, u8),
        max_cells: usize,
    ) -> Grid {
        let hi = band.1.min(keys.max_pitch());
        let lo = band.0.min(hi);
        let rows = usize::from(hi - lo) + 1;
        Grid::build_rows(
            keys,
            notes,
            idx,
            (t0, t1),
            cell,
            1,
            pad,
            (usize::from(lo), rows),
            max_cells,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_rows(
        keys: KeyRange,
        notes: &[TNote],
        idx: &[usize],
        (t0, t1): (u64, u64),
        cell: u64,
        row_h: u8,
        pad: usize,
        (row0, h): (usize, usize),
        max_cells: usize,
    ) -> Grid {
        let span = t1.saturating_sub(t0).max(1);
        let max_w = (max_cells / h).max(8);
        let mut cell = cell.max(1);
        let cols_for = |c: u64| span.div_ceil(c) as usize + 2 * pad;
        if cols_for(cell) > max_w {
            let usable = (max_w - 2 * pad.min(max_w / 4)).max(1) as u64;
            cell = span.div_ceil(usable).max(cell);
        }
        let pad = pad.min(max_w / 4);
        // Saturating: a coarse cell over a span of the whole tick range
        // times the padding passes u64 (V09-S50's harness).
        let t0 = t0.saturating_sub((pad as u64).saturating_mul(cell));
        let w = (t1.saturating_sub(t0).max(1).div_ceil(cell) as usize)
            .saturating_add(pad)
            .clamp(1, max_w);
        let mut g = Grid {
            t0,
            cell,
            row_h,
            top: keys.max_pitch(),
            row0,
            w,
            h,
            rep: vec![0; w * h],
            count: vec![0; w * h],
        };
        let mut filled: Vec<i64> = vec![-1; h];
        let mut diff: Vec<i64> = vec![0; (w + 1) * h];
        for &i in idx {
            let n = &notes[i];
            let Some((c0, c1)) = g.cols(n.start, n.end()) else {
                continue;
            };
            let r = g.row_of(n.pitch);
            diff[r * (w + 1) + c0] += 1;
            diff[r * (w + 1) + c1 + 1] -= 1;
            let from = (filled[r] + 1).max(c0 as i64) as usize;
            for c in from..=c1 {
                g.rep[r * w + c] = i as u32 + 1;
            }
            filled[r] = filled[r].max(c1 as i64);
        }
        for r in 0..h {
            let mut acc = 0i64;
            for c in 0..w {
                acc += diff[r * (w + 1) + c];
                g.count[r * w + c] = acc.max(0) as u32;
            }
        }
        g
    }

    /// Column range `[c0, c1]` covered by ticks `[s, e)` (None outside).
    pub fn cols(&self, s: u64, e: u64) -> Option<(usize, usize)> {
        if e <= self.t0 {
            return None;
        }
        let c0 = (s.saturating_sub(self.t0) / self.cell) as usize;
        let c1 = ((e - 1).saturating_sub(self.t0) / self.cell) as usize;
        if c0 >= self.w {
            return None;
        }
        Some((c0, c1.min(self.w - 1)))
    }

    #[inline]
    pub fn row_of(&self, pitch: u8) -> usize {
        (usize::from(pitch) / usize::from(self.row_h))
            .saturating_sub(self.row0)
            .min(self.h - 1)
    }

    /// The pitch of row `r` (its lowest semitone).
    #[inline]
    pub fn pitch_of(&self, r: usize) -> u8 {
        ((r + self.row0) * usize::from(self.row_h)).min(usize::from(self.top)) as u8
    }

    #[inline]
    pub fn tick_of(&self, c: usize) -> u64 {
        self.t0.saturating_add((c as u64).saturating_mul(self.cell))
    }

    /// Representative at (c, r) with bounds (0 outside).
    #[inline]
    pub fn at(&self, v: &[u32], c: isize, r: isize) -> u32 {
        if c < 0 || r < 0 || c as usize >= self.w || r as usize >= self.h {
            0
        } else {
            v[r as usize * self.w + c as usize]
        }
    }

    /// Calls `f(row, c0, c1, rep)` for every horizontal run of non-empty cells
    /// in `v` (a run also breaks where the representative's subchannel layer
    /// changes, as decided by `same`).
    pub fn runs(
        &self,
        v: &[u32],
        same: impl Fn(u32, u32) -> bool,
        mut f: impl FnMut(usize, usize, usize, u32),
    ) {
        for r in 0..self.h {
            let row = &v[r * self.w..(r + 1) * self.w];
            let mut c = 0;
            while c < self.w {
                if row[c] == 0 {
                    c += 1;
                    continue;
                }
                let start = c;
                let rep = row[c];
                while c + 1 < self.w && row[c + 1] != 0 && same(rep, row[c + 1]) {
                    c += 1;
                }
                f(r, start, c, rep);
                c += 1;
            }
        }
    }

    /// Cells of `v` that are empty and connected (4-neighbour) to the grid's
    /// border through empty cells: the outside.
    pub fn outside(&self, v: &[u32]) -> Vec<bool> {
        let (w, h) = (self.w, self.h);
        let mut out = vec![false; w * h];
        let mut stack: Vec<usize> = Vec::new();
        let push = |i: usize, out: &mut Vec<bool>, stack: &mut Vec<usize>| {
            if v[i] == 0 && !out[i] {
                out[i] = true;
                stack.push(i);
            }
        };
        for c in 0..w {
            push(c, &mut out, &mut stack);
            push((h - 1) * w + c, &mut out, &mut stack);
        }
        for r in 0..h {
            push(r * w, &mut out, &mut stack);
            push(r * w + w - 1, &mut out, &mut stack);
        }
        while let Some(i) = stack.pop() {
            let (c, r) = (i % w, i / w);
            if c > 0 {
                push(i - 1, &mut out, &mut stack);
            }
            if c + 1 < w {
                push(i + 1, &mut out, &mut stack);
            }
            if r > 0 {
                push(i - w, &mut out, &mut stack);
            }
            if r + 1 < h {
                push(i + w, &mut out, &mut stack);
            }
        }
        out
    }

    /// City-block distance of every occupied cell to the nearest empty cell
    /// (0 for empty cells), by two raster passes.
    pub fn distance(&self, v: &[u32]) -> Vec<u32> {
        let (w, h) = (self.w, self.h);
        let big = u32::MAX / 4;
        let mut d: Vec<u32> = v.iter().map(|&x| if x == 0 { 0 } else { big }).collect();
        for r in 0..h {
            for c in 0..w {
                let i = r * w + c;
                if d[i] == 0 {
                    continue;
                }
                let up = if r > 0 { d[i - w] } else { 0 };
                let left = if c > 0 { d[i - 1] } else { 0 };
                d[i] = d[i].min(up.min(left) + 1);
            }
        }
        for r in (0..h).rev() {
            for c in (0..w).rev() {
                let i = r * w + c;
                if d[i] == 0 {
                    continue;
                }
                let down = if r + 1 < h { d[i + w] } else { 0 };
                let right = if c + 1 < w { d[i + 1] } else { 0 };
                d[i] = d[i].min(down.min(right) + 1);
            }
        }
        d
    }

    /// Dilation by `k` cells (8-neighbour square steps): new cells take a
    /// neighbour's representative.
    pub fn dilate(&self, v: &[u32], k: usize) -> Vec<u32> {
        self.dilate_rings(v, k).0
    }

    /// [`Grid::dilate`] and, per cell, the step `1..=k` that set it (0 = set
    /// before or never). Frontier-based: a step visits only the empty
    /// neighbours of the cells set by the previous step (the only cells that
    /// can change), so the cost follows the cells that change instead of
    /// `k` × the whole grid; each new cell takes the first set neighbour in
    /// the same scan order as before, so results are identical.
    pub fn dilate_rings(&self, v: &[u32], k: usize) -> (Vec<u32>, Vec<u32>) {
        let (w, h) = (self.w, self.h);
        let mut cur = v.to_vec();
        let mut ring = vec![0u32; v.len()];
        let mut frontier: Vec<usize> = (0..v.len()).filter(|&i| v[i] != 0).collect();
        let mut cand: Vec<usize> = Vec::new();
        let mut vals: Vec<u32> = Vec::new();
        for step in 1..=k.min(u32::MAX as usize) as u32 {
            cand.clear();
            for &i in &frontier {
                let (r, c) = ((i / w) as isize, (i % w) as isize);
                for dr in -1..=1isize {
                    for dc in -1..=1isize {
                        let (rr, cc) = (r + dr, c + dc);
                        if rr < 0 || cc < 0 || rr as usize >= h || cc as usize >= w {
                            continue;
                        }
                        let j = rr as usize * w + cc as usize;
                        // Every candidate has a set neighbour (the frontier
                        // cell), so it is set in this step: `ring` doubles as
                        // the visited mark.
                        if cur[j] == 0 && ring[j] == 0 {
                            ring[j] = step;
                            cand.push(j);
                        }
                    }
                }
            }
            if cand.is_empty() {
                break;
            }
            // Values from the grid as it was before this step.
            vals.clear();
            for &j in &cand {
                let (r, c) = ((j / w) as isize, (j % w) as isize);
                let mut x = 0;
                'n: for dr in -1..=1 {
                    for dc in -1..=1 {
                        let y = self.at(&cur, c + dc, r + dr);
                        if y != 0 {
                            x = y;
                            break 'n;
                        }
                    }
                }
                vals.push(x);
            }
            for (&j, &x) in cand.iter().zip(&vals) {
                cur[j] = x;
            }
            std::mem::swap(&mut frontier, &mut cand);
        }
        (cur, ring)
    }

    /// Erosion by `k` cells (a cell survives when its 8 neighbours are set).
    pub fn erode(&self, v: &[u32], k: usize) -> Vec<u32> {
        let mut cur = v.to_vec();
        for _ in 0..k {
            let prev = cur.clone();
            for r in 0..self.h as isize {
                for c in 0..self.w as isize {
                    let i = r as usize * self.w + c as usize;
                    if prev[i] == 0 {
                        continue;
                    }
                    let mut all = true;
                    for dr in -1..=1 {
                        for dc in -1..=1 {
                            all &= self.at(&prev, c + dc, r + dr) != 0;
                        }
                    }
                    if !all {
                        cur[i] = 0;
                    }
                }
            }
        }
        cur
    }

    /// Majority (3 × 3) smoothing, `k` passes.
    pub fn smooth(&self, v: &[u32], k: usize) -> Vec<u32> {
        let mut cur = v.to_vec();
        for _ in 0..k {
            let prev = cur.clone();
            for r in 0..self.h as isize {
                for c in 0..self.w as isize {
                    let i = r as usize * self.w + c as usize;
                    let mut set = 0;
                    let mut rep = 0;
                    for dr in -1..=1 {
                        for dc in -1..=1 {
                            let x = self.at(&prev, c + dc, r + dr);
                            if x != 0 {
                                set += 1;
                                rep = x;
                            }
                        }
                    }
                    cur[i] = if set >= 5 {
                        if prev[i] != 0 { prev[i] } else { rep }
                    } else {
                        0
                    };
                }
            }
        }
        cur
    }

    /// Zhang–Suen thinning to one-cell-wide centre lines (iterative, bounded
    /// by the grid's thickness).
    pub fn thin(&self, v: &[u32]) -> Vec<u32> {
        let (w, h) = (self.w as isize, self.h as isize);
        let mut cur = v.to_vec();
        loop {
            let mut changed = false;
            for step in 0..2 {
                let mut kill: Vec<usize> = Vec::new();
                for r in 0..h {
                    for c in 0..w {
                        let i = (r * w + c) as usize;
                        if cur[i] == 0 {
                            continue;
                        }
                        // p2..p9 clockwise from north.
                        let nb = [
                            (0, -1),
                            (1, -1),
                            (1, 0),
                            (1, 1),
                            (0, 1),
                            (-1, 1),
                            (-1, 0),
                            (-1, -1),
                        ]
                        .map(|(dc, dr)| self.at(&cur, c + dc, r + dr) != 0);
                        let b = nb.iter().filter(|&&x| x).count();
                        if !(2..=6).contains(&b) {
                            continue;
                        }
                        let a = (0..8).filter(|&k| !nb[k] && nb[(k + 1) % 8]).count();
                        if a != 1 {
                            continue;
                        }
                        let (p2, p4, p6, p8) = (nb[0], nb[2], nb[4], nb[6]);
                        let ok = if step == 0 {
                            !(p4 && p6 && (p2 || p8))
                        } else {
                            !(p2 && p8 && (p4 || p6))
                        };
                        if ok {
                            kill.push(i);
                        }
                    }
                }
                changed |= !kill.is_empty();
                for i in kill {
                    cur[i] = 0;
                }
            }
            if !changed {
                return cur;
            }
        }
    }

    /// Number of set 8-neighbours of (c, r).
    pub fn neighbours(&self, v: &[u32], c: usize, r: usize) -> usize {
        let mut k = 0;
        for dr in -1..=1isize {
            for dc in -1..=1isize {
                if (dr, dc) != (0, 0) && self.at(v, c as isize + dc, r as isize + dr) != 0 {
                    k += 1;
                }
            }
        }
        k
    }
}

/// Exact Euclidean distance and nearest-seed (feature) transform over a
/// `w × h` cell grid (V09-S50, Tumours; row-major like [`Grid`]): a column
/// step is `ax` units, a row step `ay` units. Returns, per cell, the
/// distance to the nearest `seed` cell (`f32::INFINITY` when there is none)
/// and that seed's cell index (`u32::MAX` when there is none).
///
/// Felzenszwalb – Huttenlocher's separable transform: the nearest seed row
/// of each column (two scans), then per row the lower envelope of the
/// parabolas `(x − q)² ax² + dy(q)²` — O(w · h), no pairwise work, exact up
/// to `f64` rounding (ties keep the lower row, then the earlier column).
pub fn feature_transform(
    w: usize,
    h: usize,
    seed: &[bool],
    ax: f64,
    ay: f64,
) -> (Vec<f32>, Vec<u32>) {
    let n = w * h;
    debug_assert_eq!(seed.len(), n);
    let none = u32::MAX;
    // The nearest seed row in each cell's column.
    let mut ys = vec![none; n];
    for c in 0..w {
        let mut last = none;
        for r in 0..h {
            if seed[r * w + c] {
                last = r as u32;
            }
            ys[r * w + c] = last;
        }
        let mut next = none;
        for r in (0..h).rev() {
            let i = r * w + c;
            if seed[i] {
                next = r as u32;
            }
            if next != none {
                let cur = ys[i];
                if cur == none || (next as usize - r) < (r - cur as usize) {
                    ys[i] = next;
                }
            }
        }
    }
    let mut dist = vec![f32::INFINITY; n];
    let mut feat = vec![none; n];
    let mut v = vec![0usize; w.max(1)];
    let mut z = vec![0f64; w + 1];
    let mut f = vec![0f64; w];
    for r in 0..h {
        let row = r * w;
        let mut k: isize = -1;
        for q in 0..w {
            let y = ys[row + q];
            if y == none {
                f[q] = f64::INFINITY;
                continue;
            }
            let dy = (r as f64 - f64::from(y)) * ay;
            f[q] = dy * dy;
            let xq = q as f64 * ax;
            if k < 0 {
                k = 0;
                v[0] = q;
                z[0] = f64::NEG_INFINITY;
                z[1] = f64::INFINITY;
                continue;
            }
            loop {
                let p = v[k as usize];
                let xp = p as f64 * ax;
                let s = ((f[q] + xq * xq) - (f[p] + xp * xp)) / (2.0 * (xq - xp));
                if s <= z[k as usize] {
                    k -= 1;
                    continue;
                }
                k += 1;
                v[k as usize] = q;
                z[k as usize] = s;
                z[k as usize + 1] = f64::INFINITY;
                break;
            }
        }
        if k < 0 {
            continue;
        }
        let mut j = 0usize;
        for q in 0..w {
            let xq = q as f64 * ax;
            while z[j + 1] < xq {
                j += 1;
            }
            let p = v[j];
            let dx = xq - p as f64 * ax;
            dist[row + q] = (dx * dx + f[p]).sqrt() as f32;
            feat[row + q] = ys[row + p] * w as u32 + p as u32;
        }
    }
    (dist, feat)
}

/// Whether each of `ns` touches an empty cell (cells of `cell` ticks from
/// `t0` × one row; rows off a `keys` keyboard empty) on a side, or with
/// `eight` also on a corner — the outline of note art (Paint Color's Edge
/// paint, V09-S38; Delete's Hollow interior, V09-S39: its complement is
/// the interior). The occupied cells of each row are merged into runs
/// ([`RowMask`]), so a note is checked by binary searches: its run starts
/// or ends with it (left / right), or the rows above and below do not
/// cover its cells (one more cell each way for corners) — O(n log n),
/// whatever the span. Every note must start at or after `t0`.
pub fn cell_edges(ns: &[TNote], keys: KeyRange, t0: u64, cell: u64, eight: bool) -> Vec<bool> {
    let rows = keys.keys();
    let cell = cell.max(1);
    let cells = |n: &TNote| {
        let c0 = (n.start - t0) / cell;
        let c1 = (n.end().max(n.start + 1) - 1 - t0) / cell;
        (c0, c1 + 1)
    };
    let mut by_row: Vec<Vec<(u64, u64)>> = vec![Vec::new(); rows];
    for n in ns {
        if let Some(r) = by_row.get_mut(usize::from(n.pitch)) {
            r.push(cells(n));
        }
    }
    let m = RowMask::from_rows(by_row);
    // Whether row `p` is occupied over cells [a, b).
    let covers = |p: i64, a: u64, b: u64| -> bool {
        if p < 0 || p >= rows as i64 {
            return false;
        }
        let r = m.row(p as u8);
        let i = r.partition_point(|x| x.1 <= a);
        r.get(i).is_some_and(|x| x.0 <= a && x.1 >= b)
    };
    ns.iter()
        .map(|n| {
            let (a, b) = cells(n);
            let p = i64::from(n.pitch);
            let own = m.row(n.pitch);
            // (A note off the keyboard has no run: an edge.)
            let Some(&run) = own.get(own.partition_point(|x| x.1 <= a)) else {
                return true;
            };
            // Cell a − 1 is empty when the run starts at a (so a ≥ 1
            // below), cell b when it ends there.
            if run.0 == a || run.1 == b {
                return true;
            }
            if !covers(p - 1, a, b) || !covers(p + 1, a, b) {
                return true;
            }
            eight && (!covers(p - 1, a - 1, b + 1) || !covers(p + 1, a - 1, b + 1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dilation_by_a_box_or_a_diamond_reaches_its_rows() {
        let m = RowMask::from_notes(KeyRange::Standard, &[n(100, 100, 60)]);
        let b = m.dilate(Shape {
            kx: 10,
            ky: 2,
            diamond: false,
        });
        for p in 58..=62u8 {
            assert_eq!(b.row(p), &[(90, 210)], "box row {p}");
        }
        assert!(b.row(57).is_empty() && b.row(63).is_empty());
        let d = m.dilate(Shape {
            kx: 10,
            ky: 2,
            diamond: true,
        });
        assert_eq!(d.row(60), &[(90, 210)]);
        assert_eq!((d.row(59), d.row(61)), (&[(95, 205)][..], &[(95, 205)][..]));
        assert_eq!(
            (d.row(58), d.row(62)),
            (&[(100, 200)][..], &[(100, 200)][..])
        );
        // Clipped at tick 0 and at the keyboard's ends.
        let e = RowMask::from_notes(KeyRange::Standard, &[n(5, 10, 127)]);
        let e = e.dilate(Shape {
            kx: 10,
            ky: 1,
            diamond: false,
        });
        assert_eq!(e.row(126), &[(0, 25)]);
        assert_eq!(e.intervals(), 2);
    }

    #[test]
    fn erosion_undoes_dilation_inside_and_keyboard_ends_are_edges() {
        let mut ns = Vec::new();
        for p in 10..=20u8 {
            ns.push(n(1000, 1000, p));
        }
        let m = RowMask::from_notes(KeyRange::Standard, &ns);
        for diamond in [false, true] {
            let e = m.erode(
                Shape {
                    kx: 10,
                    ky: 1,
                    diamond,
                },
                0,
                5000,
            );
            assert!(e.row(10).is_empty() && e.row(20).is_empty());
            for p in 11..=19u8 {
                assert_eq!(e.row(p), &[(1010, 1990)], "{diamond} {p}");
            }
            // Opening of a rectangle by a box / diamond is the rectangle.
            let back = e.dilate(Shape {
                kx: 10,
                ky: 1,
                diamond: false,
            });
            assert_eq!(back.row(15), &[(1000, 2000)]);
        }
        // Art on the bottom rows: row 0 is an edge.
        let low = RowMask::from_notes(
            KeyRange::Standard,
            &[n(0, 100, 0), n(0, 100, 1), n(0, 100, 2)],
        );
        let e = low.erode(
            Shape {
                kx: 0,
                ky: 1,
                diamond: false,
            },
            0,
            100,
        );
        assert_eq!(
            (e.row(0).len(), e.row(1), e.row(2).len()),
            (0, &[(0, 100)][..], 0)
        );
        // Closing fills a gap narrower than twice the reach.
        // (Away from the window's ends: outside it counts as outside.)
        let g = RowMask::from_notes(KeyRange::Standard, &[n(100, 100, 60), n(210, 90, 60)]);
        let s = Shape {
            kx: 10,
            ky: 0,
            diamond: false,
        };
        assert_eq!(g.dilate(s).erode(s, 0, 1000).row(60), &[(100, 300)]);
        assert_eq!(g.dilate(s).erode(s, 0, 300).row(60), &[(100, 290)]);
        assert_eq!(g.clip_time(150, 250).row(60), &[(150, 200), (210, 250)]);
    }

    #[test]
    fn cell_edges_mark_the_outline_of_a_block_and_around_a_hole() {
        // A 5 × 5 block of one-cell notes (cells of 10 ticks) with the
        // centre cell missing.
        let mut ns = Vec::new();
        for r in 0..5u8 {
            for c in 0..5u64 {
                if (r, c) != (2, 2) {
                    ns.push(n(c * 10, 10, 60 + r));
                }
            }
        }
        let keys = KeyRange::default();
        let four = cell_edges(&ns, keys, 0, 10, false);
        let eight = cell_edges(&ns, keys, 0, 10, true);
        for (i, x) in ns.iter().enumerate() {
            let (c, r) = ((x.start / 10) as i64, i64::from(x.pitch) - 60);
            let border = c == 0 || c == 4 || r == 0 || r == 4;
            let side = (c - 2).abs() + (r - 2).abs() == 1;
            let corner = (c - 2).abs() == 1 && (r - 2).abs() == 1;
            assert_eq!(four[i], border || side, "{c} {r}");
            assert_eq!(eight[i], border || side || corner, "{c} {r}");
        }
        // A long note inside a row of touching notes is interior when the
        // rows above and below cover all its cells (one note each), and an
        // edge as soon as one cell above it is empty.
        let mut v = vec![
            n(0, 50, 60),
            n(0, 10, 61),
            n(10, 30, 61),
            n(40, 10, 61),
            n(0, 50, 62),
        ];
        let e = cell_edges(&v, keys, 0, 10, false);
        assert_eq!(e, vec![true, true, false, true, true]);
        v[4] = n(0, 30, 62);
        v.push(n(40, 10, 62));
        let e = cell_edges(&v, keys, 0, 10, false);
        assert!(e[2], "cell 3 of row 62 is empty");
    }

    fn n(start: u64, dur: u64, pitch: u8) -> TNote {
        TNote {
            start,
            dur,
            pitch,
            vel: 100,
            ..TNote::default()
        }
    }

    fn pieces(m: &RowMask, p: u8, s: u64, e: u64) -> Vec<(u64, u64)> {
        let mut v = Vec::new();
        m.clip(p, s, e, |a, b| v.push((a, b)));
        v
    }

    #[test]
    fn from_rows_normalizes_and_overlapping_counts_the_clipped_pieces() {
        let m = RowMask::from_rows(vec![
            vec![(50, 60), (0, 10), (10, 20), (30, 30), (35, 45)],
            vec![],
            vec![(5, 7)],
        ]);
        assert_eq!(m.row_count(), 3);
        assert_eq!(m.row(0), &[(0, 20), (35, 45), (50, 60)]);
        assert_eq!(m.extent(), Some((0, 60)));
        assert_eq!(RowMask::from_rows(vec![vec![], vec![]]).extent(), None);
        // Every window: the count equals the pieces clip gives.
        for s in 0..70u64 {
            for e in s..72 {
                for p in 0..3u8 {
                    assert_eq!(
                        m.overlapping(p, s, e),
                        pieces(&m, p, s, e).len(),
                        "row {p} [{s}, {e})"
                    );
                }
            }
        }
    }

    #[test]
    fn row_masks_union_intersect_invert_subtract_and_clip_exactly() {
        let a = RowMask::from_notes(
            KeyRange::Standard,
            &[n(0, 100, 60), n(50, 100, 60), n(300, 100, 60)],
        );
        assert_eq!(a.row(60), &[(0, 150), (300, 400)]);
        let b = RowMask::from_notes(KeyRange::Standard, &[n(100, 250, 60), n(0, 10, 61)]);
        assert_eq!(a.intersect(&b).row(60), &[(100, 150), (300, 350)]);
        assert_eq!(a.union(&b).row(60), &[(0, 400)]);
        assert_eq!(a.invert(0, 500).row(60), &[(150, 300), (400, 500)]);
        assert_eq!(a.invert(0, 500).row(61), &[(0, 500)]);
        assert_eq!(a.subtract(&b, 0, 500).row(60), &[(0, 100), (350, 400)]);
        assert_eq!(pieces(&a, 60, 120, 320), vec![(120, 150), (300, 320)]);
        assert!(pieces(&a, 62, 0, 1000).is_empty());
        let e = a.expand(10, 1);
        assert_eq!(e.row(59), &[(0, 160), (290, 410)]);
        let c = a.contract(10, 0, 0, 1000);
        assert_eq!(c.row(60), &[(10, 140), (310, 390)]);
        assert_eq!(a.edge_distance(60, 140), 10);
    }

    #[test]
    fn frontier_dilation_equals_step_by_step_dilation() {
        // Reference: the plain algorithm (scan every cell, every step).
        fn naive(g: &Grid, v: &[u32], k: usize) -> (Vec<u32>, Vec<u32>) {
            let mut cur = v.to_vec();
            let mut ring = vec![0u32; v.len()];
            for step in 1..=k as u32 {
                let prev = cur.clone();
                for r in 0..g.h as isize {
                    for c in 0..g.w as isize {
                        let i = r as usize * g.w + c as usize;
                        if prev[i] != 0 {
                            continue;
                        }
                        'n: for dr in -1..=1 {
                            for dc in -1..=1 {
                                let x = g.at(&prev, c + dc, r + dr);
                                if x != 0 {
                                    cur[i] = x;
                                    ring[i] = step;
                                    break 'n;
                                }
                            }
                        }
                    }
                }
            }
            (cur, ring)
        }
        let mut rng = crate::transform::rng::Rng::new(5);
        for round in 0..40 {
            let mut v = Vec::new();
            for _ in 0..(1 + round % 9) {
                let st = rng.range_i64(0, 400) as u64;
                let du = rng.range_i64(1, 120) as u64;
                v.push(n(st, du, rng.range_i64(40, 80) as u8));
            }
            v.sort_by_key(|x| x.start);
            let idx: Vec<usize> = (0..v.len()).collect();
            let g = Grid::build(
                KeyRange::Standard,
                &v,
                &idx,
                0,
                520,
                10,
                1 + (round % 3) as u8,
                4,
            );
            for k in [0usize, 1, 2, 3, 7, 60] {
                assert_eq!(
                    g.dilate_rings(&g.rep, k),
                    naive(&g, &g.rep, k),
                    "round {round} k {k}"
                );
            }
        }
    }

    #[test]
    fn grids_build_in_one_pass_and_morphology_behaves() {
        // A 6 × 3 block of notes (rows 60–62, 6 cells of 10 ticks) with a hole.
        let mut v = Vec::new();
        for p in 60..63u8 {
            v.push(n(0, 60, p));
        }
        let idx: Vec<usize> = (0..v.len()).collect();
        let g = Grid::build(KeyRange::Standard, &v, &idx, 0, 60, 10, 1, 2);
        assert_eq!(g.cell, 10);
        let r = g.row_of(61);
        let c0 = g.cols(0, 60).unwrap();
        assert_eq!(c0.1 - c0.0, 5);
        let mut cells = g.rep.clone();
        assert!(cells[r * g.w + c0.0] != 0 && g.count[r * g.w + c0.0] == 1);
        // Punch a hole in the middle: it is not outside.
        cells[r * g.w + c0.0 + 2] = 0;
        let out = g.outside(&cells);
        assert!(!out[r * g.w + c0.0 + 2] && out[0]);
        let d = g.distance(&g.rep);
        assert_eq!(
            d[r * g.w + c0.0 + 2],
            2,
            "middle row is two steps from the edge"
        );
        let th = g.thin(&g.rep);
        let left = th.iter().filter(|&&x| x != 0).count();
        assert!(left > 0 && left < 18, "{left}");
        assert_eq!(g.erode(&g.rep, 1).iter().filter(|&&x| x != 0).count(), 4);
        // No column left of tick 0: 7 × 5.
        assert_eq!(
            g.dilate(&g.rep, 1).iter().filter(|&&x| x != 0).count(),
            7 * 5
        );
        let mut runs = Vec::new();
        g.runs(&g.rep, |_, _| true, |row, a, b, _| runs.push((row, a, b)));
        assert_eq!(runs.len(), 3);
        // Coarsening: a huge span never exceeds the cell budget.
        let big = [n(0, 1 << 40, 64)];
        let g = Grid::build(KeyRange::Standard, &big, &[0], 0, 1 << 40, 1, 1, 4);
        assert!(g.w * g.h <= GRID_MAX_CELLS && g.cell > 1);
    }

    #[test]
    fn the_feature_transform_matches_a_brute_force_nearest_seed() {
        // Seeded random seeds on grids of several shapes and aspect ratios.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for (w, h, ax, ay, share) in [
            (17, 9, 1.0, 1.0, 9),
            (40, 23, 0.5, 1.0, 40),
            (31, 31, 2.5, 1.0, 3),
            (1, 12, 1.0, 1.0, 4),
            (12, 1, 0.75, 1.0, 4),
            (25, 14, 1.0, 1.7, 60),
        ] {
            let seed: Vec<bool> = (0..w * h).map(|_| next() % 100 < share).collect();
            let (d, f) = feature_transform(w, h, &seed, ax, ay);
            for r in 0..h {
                for c in 0..w {
                    let mut best = f64::INFINITY;
                    for (i, &s) in seed.iter().enumerate() {
                        if s {
                            let dx = (c as f64 - (i % w) as f64) * ax;
                            let dy = (r as f64 - (i / w) as f64) * ay;
                            best = best.min(dx.hypot(dy));
                        }
                    }
                    let i = r * w + c;
                    if best.is_infinite() {
                        assert!(d[i].is_infinite() && f[i] == u32::MAX);
                        continue;
                    }
                    assert!(
                        (f64::from(d[i]) - best).abs() < 1e-4,
                        "{w}x{h} ({c},{r}): {} vs {best}",
                        d[i]
                    );
                    let fi = f[i] as usize;
                    assert!(seed[fi], "the feature is a seed");
                    let dx = (c as f64 - (fi % w) as f64) * ax;
                    let dy = (r as f64 - (fi / w) as f64) * ay;
                    assert!(
                        (dx.hypot(dy) - best).abs() < 1e-4,
                        "the feature is a nearest seed"
                    );
                }
            }
        }
        // No seed at all.
        let (d, f) = feature_transform(3, 2, &[false; 6], 1.0, 1.0);
        assert!(d.iter().all(|v| v.is_infinite()) && f.iter().all(|&v| v == u32::MAX));
    }
}
