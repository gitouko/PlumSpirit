//! Interaction models of the parameter editors (v0.8 §182–§188): what a press,
//! a drag, a release, Delete or a typed number does to the values, with no
//! egui in sight — so pointer interactions are unit-tested exactly as the
//! widgets drive them. Screen coordinates are points (y down); a press always
//! acts at its *press origin* (egui reports drags only after a threshold, by
//! which time the pointer has moved off small handles — the v0.7 curve
//! editor's "points do not move" bug); hit radii are in screen points, so
//! the editors behave the same at every zoom / window size.

/// Hit radius of a handle (screen points).
pub const HIT: f32 = 9.0;

/// How far (screen points) the pointer must move from the press origin
/// before a grabbed point moves (v0.9 §106): a click selects without nudging.
pub const DRAG_THRESHOLD: f32 = 3.0;

/// A screen rectangle and the value ranges it shows (y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Map {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub x: (f64, f64),
    pub y: (f64, f64),
}

impl Map {
    pub fn to_screen(self, x: f64, y: f64) -> (f32, f32) {
        let fx = (x - self.x.0) / (self.x.1 - self.x.0);
        let fy = (y - self.y.0) / (self.y.1 - self.y.0);
        (
            self.left + (self.right - self.left) * fx as f32,
            self.bottom - (self.bottom - self.top) * fy as f32,
        )
    }

    pub fn to_value(self, sx: f32, sy: f32) -> (f64, f64) {
        let fx = f64::from((sx - self.left) / (self.right - self.left));
        let fy = f64::from((self.bottom - sy) / (self.bottom - self.top));
        (
            self.x.0 + (self.x.1 - self.x.0) * fx,
            self.y.0 + (self.y.1 - self.y.0) * fy,
        )
    }
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Constraints of a point set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Order {
    /// Points anywhere in the bounds.
    Free,
    /// A curve: sorted by x, every point (the ends too, v0.9 §104) free
    /// within the bounds and strictly between its neighbours.
    Curve,
}

/// Draggable points (curves, point lists, quads, boxes, meshes).
#[derive(Clone, Debug, PartialEq)]
pub struct Points {
    pub pts: Vec<(f64, f64)>,
    pub selected: Option<usize>,
    pub dragging: Option<usize>,
    pub order: Order,
    /// Value bounds (x, y).
    pub bx: (f64, f64),
    pub by: (f64, f64),
    pub min_points: usize,
    pub max_points: usize,
    /// Snap step (values), e.g. Shift in the curve editor.
    pub snap: Option<f64>,
    /// Whether points can be added / removed (quads, boxes: fixed).
    pub fixed: bool,
    /// Press origin of the current grab (screen points).
    pub origin: Option<(f32, f32)>,
    /// Whether the grab passed [`DRAG_THRESHOLD`] (the point follows).
    pub armed: bool,
}

impl Points {
    pub fn new(pts: Vec<(f64, f64)>, order: Order, bx: (f64, f64), by: (f64, f64)) -> Points {
        Points {
            pts,
            selected: None,
            dragging: None,
            order,
            bx,
            by,
            min_points: if order == Order::Curve { 2 } else { 0 },
            max_points: 4096,
            snap: None,
            fixed: false,
            origin: None,
            armed: false,
        }
    }

    /// The nearest point within [`HIT`] of the screen position.
    pub fn hit(&self, m: &Map, sx: f32, sy: f32) -> Option<usize> {
        self.pts
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| (i, dist(m.to_screen(x, y), (sx, sy))))
            .filter(|(_, d)| *d <= HIT)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    fn snapped(&self, v: f64) -> f64 {
        match self.snap {
            Some(s) if s > 0.0 => (v / s).round() * s,
            _ => v,
        }
    }

    /// Where point `i` may go for a requested (x, y).
    fn constrain(&self, i: usize, x: f64, y: f64) -> (f64, f64) {
        let mut x = self.snapped(x).clamp(self.bx.0, self.bx.1);
        let y = self.snapped(y).clamp(self.by.0, self.by.1);
        if self.order == Order::Curve {
            // Between the neighbours (the bounds for an end); no point is
            // pinned.
            let n = self.pts.len();
            let eps = self.eps();
            let lo = if i > 0 {
                self.pts[i - 1].0 + eps
            } else {
                self.bx.0
            };
            let hi = if i + 1 < n {
                self.pts[i + 1].0 - eps
            } else {
                self.bx.1
            };
            x = if lo <= hi {
                x.clamp(lo, hi)
            } else {
                (lo + hi) / 2.0
            };
        }
        (x, y)
    }

    /// Smallest x gap kept between curve points.
    fn eps(&self) -> f64 {
        (self.bx.1 - self.bx.0) * 1e-3
    }

    /// Press at the press origin: selects (and grabs) the point under it.
    pub fn press(&mut self, m: &Map, sx: f32, sy: f32) -> bool {
        let h = self.hit(m, sx, sy);
        self.selected = h;
        self.dragging = h;
        self.origin = h.map(|_| (sx, sy));
        self.armed = false;
        h.is_some()
    }

    /// Pointer moved to (sx, sy) while pressed: moves the grabbed point once
    /// the pointer is [`DRAG_THRESHOLD`] away from the press origin.
    pub fn drag_to(&mut self, m: &Map, sx: f32, sy: f32) -> bool {
        let Some(i) = self.dragging else {
            return false;
        };
        if !self.armed {
            if let Some(o) = self.origin
                && dist(o, (sx, sy)) < DRAG_THRESHOLD
            {
                return false;
            }
            self.armed = true;
        }
        let (x, y) = m.to_value(sx, sy);
        let p = self.constrain(i, x, y);
        if self.pts[i] != p {
            self.pts[i] = p;
            return true;
        }
        false
    }

    pub fn release(&mut self) {
        self.dragging = None;
        self.origin = None;
        self.armed = false;
    }

    /// Adds a point at the screen position (double-click) and selects it.
    pub fn add_at(&mut self, m: &Map, sx: f32, sy: f32) -> bool {
        if self.fixed || self.pts.len() >= self.max_points || self.hit(m, sx, sy).is_some() {
            return false;
        }
        let (x, y) = m.to_value(sx, sy);
        self.add_value(x, y)
    }

    /// Adds a point at value (x, y).
    pub fn add_value(&mut self, x: f64, y: f64) -> bool {
        if self.fixed || self.pts.len() >= self.max_points {
            return false;
        }
        let (x, y) = (
            self.snapped(x).clamp(self.bx.0, self.bx.1),
            self.snapped(y).clamp(self.by.0, self.by.1),
        );
        let i = match self.order {
            // Anywhere: before the first point or after the last makes a new end.
            Order::Curve => self.pts.partition_point(|p| p.0 < x),
            Order::Free => self.pts.len(),
        };
        self.pts.insert(i, (x, y));
        let p = self.constrain(i, x, y);
        self.pts[i] = p;
        self.selected = Some(i);
        true
    }

    /// Adds a point in the middle of the widest gap (the "+" button).
    pub fn add_middle(&mut self) -> bool {
        match self.order {
            Order::Curve => {
                let Some(i) = (1..self.pts.len()).max_by(|&a, &b| {
                    let ga = self.pts[a].0 - self.pts[a - 1].0;
                    let gb = self.pts[b].0 - self.pts[b - 1].0;
                    ga.total_cmp(&gb)
                }) else {
                    return false;
                };
                let (a, b) = (self.pts[i - 1], self.pts[i]);
                self.add_value((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
            }
            Order::Free => {
                self.add_value((self.bx.0 + self.bx.1) / 2.0, (self.by.0 + self.by.1) / 2.0)
            }
        }
    }

    /// Removes the selected point (a curve end too, never below the minimum).
    pub fn delete_selected(&mut self) -> bool {
        let Some(i) = self.selected.filter(|&i| i < self.pts.len()) else {
            return false;
        };
        if self.fixed || self.pts.len() <= self.min_points {
            return false;
        }
        self.pts.remove(i);
        self.selected = None;
        self.dragging = None;
        true
    }

    /// Whether [`Points::delete_selected`] would remove a point.
    pub fn can_delete(&self) -> bool {
        self.selected.is_some_and(|i| i < self.pts.len())
            && !self.fixed
            && self.pts.len() > self.min_points
    }

    /// A copy of the selected point, selected: a curve's copy goes halfway
    /// into the wider gap beside it (same y), a free point's copy a little
    /// to the right.
    pub fn duplicate_selected(&mut self) -> bool {
        let Some(i) = self.selected.filter(|&i| i < self.pts.len()) else {
            return false;
        };
        if self.fixed || self.pts.len() >= self.max_points {
            return false;
        }
        let (x, y) = self.pts[i];
        match self.order {
            Order::Curve => {
                let next = self.pts.get(i + 1).map_or(self.bx.1, |p| p.0);
                let prev = if i > 0 { self.pts[i - 1].0 } else { self.bx.0 };
                let (right, left) = (next - x, x - prev);
                if right.max(left) < 2.0 * self.eps() {
                    return false;
                }
                if right >= left {
                    self.pts.insert(i + 1, (x + right / 2.0, y));
                    self.selected = Some(i + 1);
                } else {
                    self.pts.insert(i, (x - left / 2.0, y));
                    self.selected = Some(i);
                }
            }
            Order::Free => {
                let d = (self.bx.1 - self.bx.0) * 0.02;
                let nx = if x + d <= self.bx.1 { x + d } else { x - d };
                self.pts.insert(i + 1, (nx, y));
                self.selected = Some(i + 1);
            }
        }
        true
    }

    /// Typed coordinates of the selected point.
    pub fn set_selected(&mut self, x: f64, y: f64) -> bool {
        let Some(i) = self.selected else {
            return false;
        };
        let p = self.constrain(i, x, y);
        if self.pts[i] != p {
            self.pts[i] = p;
            return true;
        }
        false
    }
}

/// Bars: one value per entry (sequences, intervals, subchannel cycles).
#[derive(Clone, Debug, PartialEq)]
pub struct Bars {
    pub vals: Vec<i64>,
    pub min: i64,
    pub max: i64,
    pub max_len: usize,
    pub selected: Option<usize>,
    /// Entry painted last during a drag (values between are interpolated).
    pub last: Option<usize>,
}

impl Bars {
    pub fn new(vals: Vec<i64>, min: i64, max: i64, max_len: usize) -> Bars {
        Bars {
            vals,
            min,
            max,
            max_len,
            selected: None,
            last: None,
        }
    }

    /// Entry under screen x (equal columns over the map's width).
    pub fn index_at(&self, m: &Map, sx: f32) -> Option<usize> {
        let n = self.vals.len();
        if n == 0 || sx < m.left || sx > m.right {
            return None;
        }
        let f = (sx - m.left) / (m.right - m.left);
        Some(((f * n as f32) as usize).min(n - 1))
    }

    /// Value at screen y (the map's y range), rounded and clamped.
    pub fn value_at(&self, m: &Map, sy: f32) -> i64 {
        let (_, y) = m.to_value(m.left, sy);
        (y.round() as i64).clamp(self.min, self.max)
    }

    pub fn press(&mut self, m: &Map, sx: f32, sy: f32) -> bool {
        let Some(i) = self.index_at(m, sx) else {
            return false;
        };
        self.selected = Some(i);
        self.last = Some(i);
        let v = self.value_at(m, sy);
        let changed = self.vals[i] != v;
        self.vals[i] = v;
        changed
    }

    /// Drag: sets the entry under the pointer, filling skipped entries
    /// linearly so fast strokes leave no gaps.
    pub fn drag_to(&mut self, m: &Map, sx: f32, sy: f32) -> bool {
        let Some(last) = self.last else {
            return false;
        };
        let sx = sx.clamp(m.left, m.right);
        let Some(i) = self.index_at(m, sx) else {
            return false;
        };
        let v = self.value_at(m, sy);
        let mut changed = false;
        let (a, b) = (last.min(i), last.max(i));
        let va = self.vals[last];
        for k in a..=b {
            let t = if b == a {
                1.0
            } else if i >= last {
                (k - a) as f64 / (b - a) as f64
            } else {
                (b - k) as f64 / (b - a) as f64
            };
            let nv = if k == last && k != i {
                va
            } else {
                ((va as f64) + (v - va) as f64 * t).round() as i64
            };
            if self.vals[k] != nv {
                self.vals[k] = nv;
                changed = true;
            }
        }
        self.selected = Some(i);
        self.last = Some(i);
        changed
    }

    pub fn release(&mut self) {
        self.last = None;
    }

    pub fn push(&mut self, v: i64) -> bool {
        if self.vals.len() >= self.max_len {
            return false;
        }
        self.vals.push(v.clamp(self.min, self.max));
        self.selected = Some(self.vals.len() - 1);
        true
    }

    pub fn pop(&mut self) -> bool {
        if self.vals.pop().is_some() {
            self.selected = self.selected.filter(|&s| s < self.vals.len());
            return true;
        }
        false
    }

    pub fn set_selected(&mut self, v: i64) -> bool {
        let Some(i) = self.selected.filter(|&i| i < self.vals.len()) else {
            return false;
        };
        let v = v.clamp(self.min, self.max);
        let changed = self.vals[i] != v;
        self.vals[i] = v;
        changed
    }
}

/// Toggle cells (rhythm steps, sets): a press flips the cell and a drag paints
/// that new state over the cells it passes.
#[derive(Clone, Debug, PartialEq)]
pub struct Cells {
    pub on: Vec<bool>,
    pub paint: Option<bool>,
}

impl Cells {
    pub fn index_at(&self, m: &Map, sx: f32) -> Option<usize> {
        let n = self.on.len();
        if n == 0 || sx < m.left || sx > m.right {
            return None;
        }
        let f = (sx - m.left) / (m.right - m.left);
        Some(((f * n as f32) as usize).min(n - 1))
    }

    pub fn press(&mut self, m: &Map, sx: f32) -> bool {
        let Some(i) = self.index_at(m, sx) else {
            return false;
        };
        self.on[i] = !self.on[i];
        self.paint = Some(self.on[i]);
        true
    }

    pub fn drag_to(&mut self, m: &Map, sx: f32) -> bool {
        let (Some(state), Some(i)) = (self.paint, self.index_at(m, sx)) else {
            return false;
        };
        let changed = self.on[i] != state;
        self.on[i] = state;
        changed
    }

    pub fn release(&mut self) {
        self.paint = None;
    }
}

/// Pairs of ints → points and back (flat lists "x, y, x, y, …").
pub fn pairs(v: &[i64], per: usize) -> Vec<(f64, f64)> {
    v.chunks(per.max(2))
        .filter(|c| c.len() >= 2)
        .map(|c| (c[0] as f64, c[1] as f64))
        .collect()
}

/// Writes moved points back into the flat list (extra values kept).
pub fn write_pairs(v: &mut Vec<i64>, pts: &[(f64, f64)], per: usize) {
    let per = per.max(2);
    let mut out = Vec::with_capacity(pts.len() * per);
    for (k, p) in pts.iter().enumerate() {
        out.push(p.0.round() as i64);
        out.push(p.1.round() as i64);
        for e in 2..per {
            out.push(v.get(k * per + e).copied().unwrap_or(0));
        }
    }
    *v = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> Map {
        // 200 × 100 points at (10, 10); x and y 0 … 1.
        Map {
            left: 10.0,
            top: 10.0,
            right: 210.0,
            bottom: 110.0,
            x: (0.0, 1.0),
            y: (0.0, 1.0),
        }
    }

    #[test]
    fn a_curve_point_is_grabbed_at_the_press_origin_and_follows_the_drag() {
        let m = map();
        let mut c = Points::new(
            vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)],
            Order::Curve,
            (0.0, 1.0),
            (0.0, 1.0),
        );
        // The middle point is at (110, 60). Press 5 points away (inside the
        // hit radius); egui reports the drag only after the pointer moved
        // further — the point is already grabbed.
        assert!(c.press(&m, 113.0, 64.0));
        assert_eq!(c.dragging, Some(1));
        assert!(c.drag_to(&m, 150.0, 30.0));
        let (x, y) = c.pts[1];
        assert!((x - 0.7).abs() < 1e-5 && (y - 0.8).abs() < 1e-5, "{x} {y}");
        // Interior points stay between their neighbours.
        c.drag_to(&m, 300.0, -50.0);
        assert!(c.pts[1].0 < 1.0 && c.pts[1].1 == 1.0);
        c.release();
        // v0.9 §104: the first point moves away from x = 0 and stays there.
        assert!(c.press(&m, 10.0, 110.0));
        c.drag_to(&m, 60.0, 80.0);
        assert!((c.pts[0].0 - 0.25).abs() < 1e-5, "{:?}", c.pts[0]);
        assert!((c.pts[0].1 - 0.3).abs() < 1e-5);
        c.release();
        // ... and the last one inward, never past its neighbour.
        c.pts[1] = (0.5, 0.5);
        assert!(c.press(&m, 210.0, 10.0));
        c.drag_to(&m, 170.0, 30.0);
        assert!((c.pts[2].0 - 0.8).abs() < 1e-5, "{:?}", c.pts[2]);
        c.drag_to(&m, 0.0, 30.0);
        assert!(c.pts[2].0 > c.pts[1].0);
        c.release();
        // Pressing empty space selects nothing and drags nothing.
        assert!(!c.press(&m, 60.0, 15.0));
        assert!(!c.drag_to(&m, 70.0, 20.0));
    }

    #[test]
    fn curve_points_are_added_deleted_typed_snapped_and_reset_safely() {
        let m = map();
        let mut c = Points::new(
            vec![(0.0, 0.0), (1.0, 1.0)],
            Order::Curve,
            (0.0, 1.0),
            (0.0, 1.0),
        );
        // Double-click in empty space adds a sorted, selected point.
        assert!(c.add_at(&m, 60.0, 60.0));
        assert_eq!(c.pts.len(), 3);
        assert_eq!(c.selected, Some(1));
        assert!((c.pts[1].0 - 0.25).abs() < 1e-5);
        // Not on top of an existing point.
        assert!(!c.add_at(&m, 61.0, 61.0));
        // Typed coordinates are constrained like a drag.
        assert!(c.set_selected(2.0, 0.4));
        assert!(c.pts[1].0 < 1.0 && (c.pts[1].1 - 0.4).abs() < 1e-5);
        // Any point can be deleted (v0.9 §104: ends too), down to two.
        c.selected = Some(0);
        assert!(c.can_delete() && c.delete_selected());
        assert_eq!(c.pts.len(), 2);
        assert_eq!(c.pts[1], (1.0, 1.0));
        assert!(
            (c.pts[0].1 - 0.4).abs() < 1e-5,
            "the typed point is now first"
        );
        c.selected = Some(1);
        assert!(!c.can_delete() && !c.delete_selected());
        c.pts = vec![(0.0, 0.0), (1.0, 1.0)];
        // "+" adds in the widest gap.
        assert!(c.add_middle());
        assert!((c.pts[1].0 - 0.5).abs() < 1e-5);
        // Snap.
        c.snap = Some(0.25);
        c.press(&m, 110.0, 60.0);
        c.drag_to(&m, 128.0, 52.0);
        assert_eq!(c.pts[1], (0.5, 0.5));
    }

    #[test]
    fn hit_testing_is_in_screen_points_at_any_size() {
        let mut m = map();
        let c = Points::new(vec![(0.5, 0.5)], Order::Free, (0.0, 1.0), (0.0, 1.0));
        assert_eq!(c.hit(&m, 110.0 + HIT - 0.5, 60.0), Some(0));
        assert_eq!(c.hit(&m, 110.0 + HIT + 0.5, 60.0), None);
        // Ten times larger: the same radius in points.
        m.right = 2010.0;
        m.bottom = 1010.0;
        assert_eq!(c.hit(&m, 1010.0 + HIT - 0.5, 510.0), Some(0));
        assert_eq!(c.hit(&m, 1010.0 + HIT + 0.5, 510.0), None);
    }

    #[test]
    fn bars_paint_without_gaps_and_clamp() {
        let m = Map {
            y: (0.0, 100.0),
            ..map()
        };
        let mut b = Bars::new(vec![0; 10], 0, 100, 16);
        // Column width 20 points; press column 1 at value 50.
        assert!(b.press(&m, 35.0, 60.0));
        assert_eq!(b.vals[1], 50);
        // A fast stroke to column 7 at value 80 fills 2 … 7.
        assert!(b.drag_to(&m, 155.0, 30.0));
        assert_eq!(&b.vals[1..8], &[50, 55, 60, 65, 70, 75, 80]);
        // Beyond the top clamps.
        b.drag_to(&m, 175.0, -100.0);
        assert_eq!(b.vals[8], 100);
        b.release();
        assert!(!b.drag_to(&m, 20.0, 50.0), "no drag without a press");
        assert!(b.push(500));
        assert_eq!(*b.vals.last().unwrap(), 100);
        b.selected = Some(1);
        assert!(b.set_selected(-7));
        assert_eq!(b.vals[1], 0);
    }

    #[test]
    fn cells_toggle_then_paint_the_same_state() {
        let m = map();
        let mut c = Cells {
            on: vec![false, true, false, true],
            paint: None,
        };
        assert!(c.press(&m, 20.0));
        assert!(c.on[0]);
        c.drag_to(&m, 70.0);
        c.drag_to(&m, 120.0);
        c.drag_to(&m, 170.0);
        assert_eq!(c.on, vec![true; 4]);
        c.release();
        assert!(!c.drag_to(&m, 20.0));
    }

    #[test]
    fn point_lists_round_trip_and_keep_extra_values() {
        let mut v = vec![250, 250, 100, 0, 750, 750, -100, 0];
        let mut pts = pairs(&v, 4);
        assert_eq!(pts, vec![(250.0, 250.0), (750.0, 750.0)]);
        pts[1] = (700.4, 800.6);
        write_pairs(&mut v, &pts, 4);
        assert_eq!(v, vec![250, 250, 100, 0, 700, 801, -100, 0]);
        pts.push((10.0, 20.0));
        write_pairs(&mut v, &pts, 4);
        assert_eq!(&v[8..], &[10, 20, 0, 0]);
    }

    #[test]
    fn a_press_selects_without_nudging_until_the_drag_threshold() {
        // v0.9 §106 / §251: press, small moves (jitter) leave the point where
        // it was; past the threshold it follows; release ends the grab.
        let m = map();
        let mut c = Points::new(
            vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)],
            Order::Curve,
            (0.0, 1.0),
            (0.0, 1.0),
        );
        assert!(c.press(&m, 112.0, 61.0));
        assert_eq!(c.selected, Some(1));
        assert!(!c.drag_to(&m, 113.5, 62.0));
        assert!(!c.drag_to(&m, 112.0 + DRAG_THRESHOLD - 0.1, 61.0));
        assert_eq!(c.pts[1], (0.5, 0.5));
        assert!(c.drag_to(&m, 112.0 + DRAG_THRESHOLD + 0.1, 61.0));
        // Once armed, small moves follow too.
        assert!(c.drag_to(&m, 113.0, 60.0));
        assert!((c.pts[1].0 - 0.515).abs() < 1e-5);
        c.release();
        assert!(c.dragging.is_none() && c.origin.is_none() && !c.armed);
        assert!(!c.drag_to(&m, 150.0, 30.0));
        assert_eq!(c.selected, Some(1), "release keeps the selection");
    }

    #[test]
    fn curve_points_are_added_past_the_ends_and_duplicated_into_the_wider_gap() {
        let m = map();
        let mut c = Points::new(
            vec![(0.2, 0.0), (0.8, 1.0)],
            Order::Curve,
            (0.0, 1.0),
            (0.0, 1.0),
        );
        // Double-click left of the first point: a new first point.
        assert!(c.add_at(&m, 30.0, 60.0));
        assert_eq!(c.selected, Some(0));
        assert!((c.pts[0].0 - 0.1).abs() < 1e-5);
        // Right of the last: a new last point.
        assert!(c.add_value(0.95, 0.3));
        assert_eq!(c.selected, Some(3));
        assert_eq!(c.pts.len(), 4);
        // Duplicate: the copy goes into the wider gap (0.2 → 0.8), same y.
        c.selected = Some(1);
        assert!(c.duplicate_selected());
        assert_eq!(c.selected, Some(2));
        assert_eq!(c.pts[2], (0.5, 0.0));
        // The last point's wider gap is to its left.
        c.selected = Some(4);
        assert!(c.duplicate_selected());
        assert_eq!(c.selected, Some(4));
        assert!((c.pts[4].0 - 0.875).abs() < 1e-9 && c.pts[4].1 == 0.3);
        assert!(c.pts.windows(2).all(|w| w[0].0 < w[1].0), "{:?}", c.pts);
        // Full: nothing more.
        c.max_points = c.pts.len();
        assert!(!c.duplicate_selected() && !c.add_value(0.6, 0.6));
    }
}
