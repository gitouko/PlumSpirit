//! Paths (V09-S55 step 0, lifted from Lines byte for byte): how a polyline
//! in frame units (x = ticks, y = pitch rows, `s` = position along it 0 – 1)
//! becomes notes — **strokes** (the pen's, one brush per piece of at most a
//! grid unit when the brush changes along the path), **bands** (a
//! thickness, or the region to the box's bottom / top / mirror line, filled
//! by scanline per x-monotone run) or **samples** (a note every so many
//! grid units, a spacing curve, a dot pattern); dashes along time;
//! gradients and an end taper along `s`. Lines and Arcs / Curves draw their
//! paths with it (`docs/GENERATORS.md` "Shared drawing").

use super::super::{Curve, TransformError};
use super::Frame;
use super::draw::{Aff, BoxG, Dash, Grad, MAX_PIECES, Pt, UNIT, pieces};
use super::pen::{Band, Brush, Draw};
use crate::midi::group_digits;
use std::cmp::Ordering;
use std::f64::consts::TAU;

/// How far band bounds are nudged up (rows; see `PathStyle::bounds`).
pub const BAND_EPS: f64 = 1e-7;

/// What c18 fills to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillTo {
    Bottom,
    Top,
    Mirror,
}

/// How a path is drawn (`None` / neutral = off).
#[derive(Clone, Debug)]
pub struct PathStyle {
    /// Band thickness in rows (1 = a stroke).
    pub thick: f64,
    /// Grid units between samples, note length (steps).
    pub every: Option<(f64, f64)>,
    /// The spacing curve, the widest spacing (grid units).
    pub spacing: Option<(Curve, f64)>,
    /// Gate, velocity and subchannel gradients along the path.
    pub grad: Grad,
    /// Dash, gap (ticks).
    pub dash: Option<(f64, f64)>,
    /// A dot pattern over the samples.
    pub dots: Option<Vec<bool>>,
    /// The ends tapered: length (fraction of the path at each end), floor
    /// (fraction).
    pub taper: Option<(f64, f64)>,
    /// The region to the box's bottom, top or mirror line.
    pub fill: Option<FillTo>,
    /// Samples kept by a share along the path (curve of `s`), by error
    /// diffusion: the shares add up sample by sample, and a sample is kept
    /// each time the sum reaches 1 — of n samples ⌊Σ d⌋ are kept.
    pub keep: Option<Curve>,
    /// The gradients' positions shaped by curves of `s` (gate, velocity,
    /// subchannel) instead of `s` itself.
    pub shape: Option<Box<[Curve; 3]>>,
    /// The velocity (and a band's half-thickness) scaled by a curve of `s`,
    /// like the taper.
    pub modulate: Option<Curve>,
    /// Samples' notes centred on their points (else starting there).
    pub centred: bool,
}

impl Default for PathStyle {
    /// A plain stroke.
    fn default() -> Self {
        PathStyle {
            thick: 1.0,
            every: None,
            spacing: None,
            grad: Grad::default(),
            dash: None,
            dots: None,
            taper: None,
            fill: None,
            keep: None,
            shape: None,
            modulate: None,
            centred: false,
        }
    }
}

impl PathStyle {
    /// Whether the brush changes along the line.
    pub fn varies(&self) -> bool {
        self.grad.varies() || self.taper.is_some() || self.modulate.is_some()
    }

    /// Whether notes are sampled along the path instead of stroked.
    pub fn sampled(&self) -> bool {
        self.every.is_some() || self.spacing.is_some() || self.dots.is_some() || self.keep.is_some()
    }

    /// Whether the line is drawn as a band (thicker than a row, or filled).
    pub fn banded(&self) -> bool {
        self.thick > 1.0 || self.fill.is_some()
    }

    /// The taper factor at `s` (1 away from the ends), times the
    /// modulation's.
    pub fn taper_at(&self, s: f64) -> f64 {
        let k = match self.taper {
            Some((len, floor)) => {
                let e = s.min(1.0 - s).max(0.0);
                let r = if len > 0.0 { (e / len).min(1.0) } else { 1.0 };
                floor + (1.0 - floor) * r
            }
            None => 1.0,
        };
        match &self.modulate {
            Some(m) => k * m.eval(s).clamp(0.0, 1.0),
            None => k,
        }
    }

    /// The brush at path position `s` (0 = the start, 1 = the end).
    pub fn brush(&self, s: f64, base: Brush) -> Brush {
        let s = if s.is_finite() {
            s.clamp(0.0, 1.0)
        } else {
            0.0
        };
        match &self.shape {
            None => self.grad.brush(s, base, self.taper_at(s)),
            Some(c) => self.grad.brush_each(
                [c[0].eval(s), c[1].eval(s), c[2].eval(s)],
                base,
                self.taper_at(s),
            ),
        }
    }

    /// Whether the sample at position `s` is kept (`keep`), with `acc`
    /// the shares summed so far.
    fn kept(&self, s: f64, acc: &mut f64) -> bool {
        match &self.keep {
            None => true,
            Some(k) => {
                *acc += k.eval(s).clamp(0.0, 1.0);
                if *acc >= 1.0 {
                    *acc -= 1.0;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// The least share `keep` gives anywhere (1 without it).
    fn least_kept(&self) -> f64 {
        self.keep.as_ref().map_or(1.0, |k| {
            let mut m = k.eval(0.0).min(k.eval(1.0));
            for &(x, _) in &k.points {
                m = m.min(k.eval(x));
            }
            // Between points a smooth curve stays within its neighbours'
            // values (monotone cubic); a sample of them guards the rest.
            for i in 0..=64 {
                m = m.min(k.eval(f64::from(i) / 64.0));
            }
            m.clamp(0.0, 1.0)
        })
    }

    /// Half the band's thickness at `s` (rows; at least half a row).
    pub fn half(&self, s: f64) -> f64 {
        let h = self.thick / 2.0;
        if self.thick > 1.0 {
            (h * self.taper_at(s)).max(0.5)
        } else {
            h
        }
    }

    /// The band at a point: the rows whose centres lie in `(lo, hi]` —
    /// returned nudged up by [`BAND_EPS`], so the pen's `[lo, hi)` scan
    /// gives a one-row band the stroke's row (`floor(y)`) even when `y` is
    /// a row boundary. Thickness: `y ± half`; c18 adds the region to the
    /// box's bottom / top edge, or to the mirror line (its own row band
    /// included).
    pub fn bounds(&self, p: Pt, g: &BoxG) -> (f64, f64) {
        let h = self.half(p.s);
        let (mut lo, mut hi) = (p.y - h, p.y + h);
        if let Some(to) = self.fill {
            let (a, b) = match to {
                FillTo::Bottom => (g.lo, p.y),
                FillTo::Top => (p.y, g.hi),
                FillTo::Mirror => {
                    let m = g.y0 + g.y1 - p.y;
                    (p.y.min(m) - h, p.y.max(m) + h)
                }
            };
            lo = lo.min(a.min(b));
            hi = hi.max(a.max(b));
        }
        (lo + BAND_EPS, hi + BAND_EPS)
    }

    /// The level whose crossings the band's corners need (c18).
    pub fn fill_level(&self, g: &BoxG) -> Option<f64> {
        self.fill.map(|t| match t {
            FillTo::Bottom => g.lo,
            FillTo::Top => g.hi,
            FillTo::Mirror => (g.y0 + g.y1) / 2.0,
        })
    }

    /// The spacing (grid units) to the next sample after position `s`.
    pub fn spacing_at(&self, s: f64) -> f64 {
        if let Some((c, max)) = &self.spacing {
            1.0 + c.eval(s.clamp(0.0, 1.0)).clamp(0.0, 1.0) * (max - 1.0)
        } else if let Some((e, _)) = self.every {
            e
        } else {
            1.0
        }
    }

    /// The dashes on the frame's grid (c11), `None` when off or without
    /// gaps.
    pub fn dash_grid(&self, f: &Frame) -> Option<Dash> {
        let (d, gap) = self.dash?;
        Dash::on_grid(d, gap, f)
    }

    /// The largest spacing between samples.
    pub fn max_spacing(&self) -> f64 {
        match (&self.spacing, self.every) {
            (Some((_, max)), _) => *max,
            (None, Some((e, _))) => e,
            _ => 1.0,
        }
    }

    /// The smallest spacing between samples.
    /// The samples drawing this path visits when shares keep them (`keep`;
    /// 0 when every visited sample draws a note): its grid length over the
    /// spacing. A share near 0 draws almost nothing, yet every sample is
    /// visited (V09-S58: such a curve over a box spanning every tick ran
    /// for more than 12 minutes) — see [`VISIT_CAP`].
    pub fn visits(&self, s: &Stats) -> f64 {
        if self.keep.is_some() && self.sampled() {
            s.len / self.min_spacing() + s.lines
        } else {
            0.0
        }
    }

    pub fn min_spacing(&self) -> f64 {
        match (&self.spacing, self.every) {
            (Some(_), _) => 1.0,
            (None, Some((e, _))) => e,
            _ => 1.0,
        }
    }

    /// Draws path `pts` (frame units; clipped to the box when `clip`; `rows`
    /// is the pen's clip band) as samples, a band or strokes; `pts` may get
    /// the points where it crosses a fill level.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        pts: &mut Vec<Pt>,
        clip: bool,
        g: &BoxG,
        f: &Frame,
        rows: &Band,
        pen: &mut dyn Draw,
        sc: &mut Scratch,
    ) -> bool {
        if self.sampled() {
            self.draw_samples(pts, clip, g, f, rows, pen)
        } else if self.banded() {
            if let Some(level) = self.fill_level(g) {
                insert_crossings(pts, level);
            }
            self.draw_band(pts, clip, g, f, rows, pen, sc)
        } else {
            self.draw_stroke(pts, clip, g, f, rows, pen, sc)
        }
    }

    /// Stroke mode: the pen's strokes (one brush per piece of at most a
    /// grid unit — up to [`MAX_PIECES`](super::draw::MAX_PIECES) per range — when the brush changes
    /// along the line).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_stroke(
        &self,
        pts: &[Pt],
        clip: bool,
        g: &BoxG,
        f: &Frame,
        rows: &Band,
        pen: &mut dyn Draw,
        sc: &mut Scratch,
    ) -> bool {
        let c = self;
        let base = f.brush;
        let dash = c.dash_grid(f);
        if pts.len() == 1 {
            let p = pts[0];
            let inside = !clip || clip_t(p, p, g).is_some();
            let on = dash.is_none_or(|d| d.on_at(p.x));
            return !(inside && on) || pen.stroke(&[(p.x, p.y)], c.brush(p.s, base));
        }
        if !clip && dash.is_none() && !c.varies() {
            sc.xy.clear();
            sc.xy.extend(pts.iter().map(|p| (p.x, p.y)));
            return pen.stroke(&sc.xy, base);
        }
        let band = (rows.lo as f64, rows.hi as f64);
        let part = sc.part.unwrap_or(STROKE_PART);
        sc.xy.clear();
        sc.brushes.clear();
        let (xy, brushes) = (&mut sc.xy, &mut sc.brushes);
        let mut ok = true;
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let l = g.len(a, b);
            ok = for_ranges(a, b, g, clip, band, dash, |t0, t1, _| {
                let k = if c.varies() { pieces(l * (t1 - t0)) } else { 1 };
                for j in 0..k {
                    let u0 = t0 + (t1 - t0) * j as f64 / k as f64;
                    let u1 = t0 + (t1 - t0) * (j + 1) as f64 / k as f64;
                    let (p0, p1) = (Pt::lerp(a, b, u0), Pt::lerp(a, b, u1));
                    let br = c.brush((p0.s + p1.s) / 2.0, base);
                    let joined = xy
                        .last()
                        .is_some_and(|&(x, y)| Pt { x, y, s: 0.0 }.near(p0));
                    if !joined {
                        if xy.len() > 1 && !pen.stroke_with(xy, brushes) {
                            return false;
                        }
                        xy.clear();
                        brushes.clear();
                        xy.push((p0.x, p0.y));
                    } else if xy.len() > part {
                        // V09-S60: a long stroke goes to the pen in parts
                        // sharing their joints (the same notes; a varying
                        // brush splits every segment into up to MAX_PIECES,
                        // which reached 2 GB for 2²¹ points).
                        let joint = xy[xy.len() - 1];
                        if !pen.stroke_part(xy, brushes) {
                            return false;
                        }
                        xy.clear();
                        brushes.clear();
                        xy.push(joint);
                    }
                    xy.push((p1.x, p1.y));
                    brushes.push(br);
                }
                true
            });
            if !ok {
                break;
            }
        }
        if ok && xy.len() > 1 {
            ok = pen.stroke_with(xy, brushes);
        }
        ok
    }

    /// Band mode: per x-monotone run, the region between its lower and
    /// upper boundary filled by scanline (one polygon per dash when the
    /// brush is constant, else one quad per piece); a vertical segment is a
    /// bar of rows.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_band(
        &self,
        pts: &[Pt],
        clip: bool,
        g: &BoxG,
        f: &Frame,
        rows: &Band,
        pen: &mut dyn Draw,
        sc: &mut Scratch,
    ) -> bool {
        let base = f.brush;
        let dash = self.dash_grid(f);
        if pts.len() == 1 {
            return self.bar(pts[0], pts[0], clip, g, dash, base, rows, pen);
        }
        let mut i = 0;
        while i + 1 < pts.len() {
            let dir = (pts[i + 1].x - pts[i].x).signum();
            if pts[i + 1].x == pts[i].x {
                if !self.bar(pts[i], pts[i + 1], clip, g, dash, base, rows, pen) {
                    return false;
                }
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while j + 1 < pts.len()
                && pts[j + 1].x != pts[j].x
                && (pts[j + 1].x - pts[j].x).signum() == dir
            {
                j += 1;
            }
            if !self.run(&pts[i..=j], clip, g, dash, base, rows, pen, sc) {
                return false;
            }
            i = j;
        }
        true
    }

    /// A vertical piece at `a.x` from `a` to `b`: the rows its band covers.
    #[allow(clippy::too_many_arguments)]
    fn bar(
        &self,
        a: Pt,
        b: Pt,
        clip: bool,
        g: &BoxG,
        dash: Option<Dash>,
        base: Brush,
        rows: &Band,
        pen: &mut dyn Draw,
    ) -> bool {
        let c = self;
        if clip && !(g.x0..=g.x1).contains(&a.x) {
            return true;
        }
        if dash.is_some_and(|d| !d.on_at(a.x)) {
            return true;
        }
        let (la, ha) = c.bounds(a, g);
        let (lb, hb) = c.bounds(b, g);
        let (mut lo, mut hi) = (la.min(lb), ha.max(hb));
        if clip {
            lo = lo.max(g.lo);
            hi = hi.min(g.hi);
        }
        let br = c.brush((a.s + b.s) / 2.0, base);
        let r0 = ((lo - 0.5).ceil() as i64).max(rows.lo);
        let r1 = ((hi - 0.5).ceil() as i64 - 1).min(rows.hi - 1);
        for r in r0..=r1 {
            if !pen.note(a.x, a.x, r, br) {
                return false;
            }
        }
        true
    }

    /// An x-monotone run of the band: its x-intervals worth drawing (inside
    /// the box when clipped; for a thickness band, where the line comes
    /// within the band's reach of the drawable rows), cut into dashes.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        run: &[Pt],
        clip: bool,
        g: &BoxG,
        dash: Option<Dash>,
        base: Brush,
        rows: &Band,
        pen: &mut dyn Draw,
        sc: &mut Scratch,
    ) -> bool {
        let c = self;
        let inc = run[run.len() - 1].x >= run[0].x;
        // A fill always reaches the box's rows (inside the band); a
        // thickness band only near its line.
        let reach = if c.fill.is_some() {
            f64::INFINITY
        } else {
            c.thick / 2.0 + 1.0
        };
        let (ylo, yhi) = (rows.lo as f64 - reach, rows.hi as f64 + reach);
        let mut spans = std::mem::take(&mut sc.ranges);
        spans.clear();
        for w in run.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (mut ta, mut tb) = (0.0f64, 1.0f64);
            let dy = b.y - a.y;
            if dy == 0.0 {
                if !(a.y >= ylo && a.y <= yhi) {
                    continue;
                }
            } else if reach.is_finite() {
                let (u, v) = ((ylo - a.y) / dy, (yhi - a.y) / dy);
                ta = ta.max(u.min(v));
                tb = tb.min(u.max(v));
                if ta.is_nan() || tb.is_nan() || ta > tb {
                    continue;
                }
            }
            let (xa, xb) = (a.x + (b.x - a.x) * ta, a.x + (b.x - a.x) * tb);
            let (mut x0, mut x1) = (xa.min(xb), xa.max(xb));
            if clip {
                x0 = x0.max(g.x0);
                x1 = x1.min(g.x1);
                if x0 > x1 {
                    continue;
                }
            }
            match spans.last_mut() {
                Some(l) if x0 <= l.1 && x1 >= l.0 => {
                    l.0 = l.0.min(x0);
                    l.1 = l.1.max(x1);
                }
                _ => spans.push((x0, x1)),
            }
        }
        let mut ok = true;
        'spans: for &(xa, xb) in &spans {
            match dash {
                None => {
                    if !self.window(run, inc, xa, xb, clip, g, base, pen, sc) {
                        ok = false;
                        break 'spans;
                    }
                }
                Some(d) => {
                    let m0 = ((xa - d.x0) / d.period).floor();
                    let m1 = ((xb - d.x0) / d.period).floor();
                    if !(m0.is_finite() && m1.is_finite()) {
                        continue;
                    }
                    let mut m = m0;
                    let mut last = f64::NAN;
                    while m <= m1 {
                        let ws = d.x0 + m * d.period;
                        if ws == last || (ws + d.on).partial_cmp(&ws) != Some(Ordering::Greater) {
                            break;
                        }
                        last = ws;
                        let (w0, w1) = (ws.max(xa), (ws + d.on).min(xb));
                        if w1 > w0 && !self.window(run, inc, w0, w1, clip, g, base, pen, sc) {
                            ok = false;
                            break 'spans;
                        }
                        m += 1.0;
                    }
                }
            }
        }
        sc.ranges = spans;
        ok
    }

    /// The band of `run` over ticks `[w0, w1]`: one polygon when the brush
    /// is constant, else quads of at most a grid unit (up to
    /// [`MAX_PIECES`](super::draw::MAX_PIECES) per segment).
    #[allow(clippy::too_many_arguments)]
    fn window(
        &self,
        run: &[Pt],
        inc: bool,
        w0: f64,
        w1: f64,
        clip: bool,
        g: &BoxG,
        base: Brush,
        pen: &mut dyn Draw,
        sc: &mut Scratch,
    ) -> bool {
        let c = self;
        // The run's point at tick x (binary search on the monotone x).
        let at = |x: f64| -> Pt {
            let k = if inc {
                run.partition_point(|p| p.x < x)
            } else {
                run.partition_point(|p| p.x > x)
            };
            if k == 0 {
                return run[0];
            }
            if k >= run.len() {
                return run[run.len() - 1];
            }
            let (p, q) = (run[k - 1], run[k]);
            let t = if q.x != p.x {
                (x - p.x) / (q.x - p.x)
            } else {
                0.0
            };
            Pt::lerp(p, q, t.clamp(0.0, 1.0))
        };
        let bounds = |p: Pt| -> (f64, f64) {
            let (lo, hi) = c.bounds(p, g);
            if clip {
                // Clamped to the box's rows; a band wholly outside them is
                // empty (an inverted band would have area).
                let (lo, hi) = (lo.max(g.lo), hi.min(g.hi));
                if hi < lo { (hi, hi) } else { (lo, hi) }
            } else {
                (lo, hi)
            }
        };
        // The run's points strictly inside the window, from w0 to w1.
        let (i0, i1) = if inc {
            (
                run.partition_point(|p| p.x <= w0),
                run.partition_point(|p| p.x < w1),
            )
        } else {
            (
                run.partition_point(|p| p.x >= w1),
                run.partition_point(|p| p.x > w0),
            )
        };
        sc.band.clear();
        sc.band.push(at(w0));
        if i0 < i1 {
            if inc {
                sc.band.extend_from_slice(&run[i0..i1]);
            } else {
                sc.band.extend(run[i0..i1].iter().rev());
            }
        }
        sc.band.push(at(w1));
        if !c.varies() {
            sc.poly.clear();
            for p in &sc.band {
                sc.poly.push((p.x, bounds(*p).1));
            }
            for p in sc.band.iter().rev() {
                sc.poly.push((p.x, bounds(*p).0));
            }
            return pen.fill(&sc.poly, base);
        }
        for w in sc.band.windows(2) {
            let (a, b) = (w[0], w[1]);
            let k = pieces(g.len(a, b));
            for j in 0..k {
                let p0 = Pt::lerp(a, b, j as f64 / k as f64);
                let p1 = Pt::lerp(a, b, (j + 1) as f64 / k as f64);
                let ((l0, h0), (l1, h1)) = (bounds(p0), bounds(p1));
                let quad = [(p0.x, l0), (p1.x, l1), (p1.x, h1), (p0.x, h0)];
                if !pen.fill(&quad, c.brush((p0.s + p1.s) / 2.0, base)) {
                    return false;
                }
            }
        }
        true
    }

    /// Sample mode: notes along the path every so many grid units (c06 /
    /// c07; one unit for c12 alone), dots and rests (c12), dashes (c11:
    /// each dash restarts the spacing at its start), each note on the row
    /// under the path — or the rows its band covers. Only the drawable
    /// rows are walked: a constant spacing skips the rest exactly, a
    /// spacing graph restarts after it.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_samples(
        &self,
        pts: &[Pt],
        clip: bool,
        g: &BoxG,
        f: &Frame,
        rows: &Band,
        pen: &mut dyn Draw,
    ) -> bool {
        let c = self;
        if c.dots.as_ref().is_some_and(|d| !d.contains(&true)) {
            return true;
        }
        let base = f.brush;
        let dash = c.dash_grid(f);
        let len = c.every.map_or(1.0, |e| e.1) * g.step;
        let constant = c.spacing.is_none();
        let sp0 = c.spacing_at(0.0);
        let plen = c.dots.as_ref().map_or(1, |d| d.len() as u64);
        let mut idx = 0u64;
        let mut acc = 0.0f64;
        let mut sample = |p: Pt, pen: &mut dyn Draw, idx: &mut u64| -> bool {
            let dot = c.dots.as_ref().is_none_or(|d| d[(*idx % plen) as usize]);
            *idx = (*idx + 1) % plen;
            if !dot || !c.kept(p.s, &mut acc) {
                return true;
            }
            let br = c.brush(p.s, base);
            let x0 = if c.centred { p.x - len / 2.0 } else { p.x };
            if !c.banded() {
                return pen.note(x0, x0 + len, p.y.floor() as i64, br);
            }
            let (mut lo, mut hi) = c.bounds(p, g);
            if clip {
                lo = lo.max(g.lo);
                hi = hi.min(g.hi);
            }
            let r0 = ((lo - 0.5).ceil() as i64).max(rows.lo);
            let r1 = ((hi - 0.5).ceil() as i64 - 1).min(rows.hi - 1);
            for r in r0..=r1 {
                if !pen.note(x0, x0 + len, r, br) {
                    return false;
                }
            }
            true
        };
        let band = (rows.lo as f64, rows.hi as f64);
        if pts.len() == 1 {
            let p = pts[0];
            let inside = (!clip || clip_t(p, p, g).is_some()) && p.y >= band.0 && p.y < band.1;
            return !(inside && dash.is_none_or(|d| d.on_at(p.x))) || sample(p, pen, &mut idx);
        }
        let mut next = 0.0f64;
        let mut acc = 0.0f64;
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let l = g.len(a, b);
            let ok = for_ranges(a, b, g, clip, band, dash, |t0, t1, fresh| {
                let (r0, r1) = (acc + t0 * l, acc + t1 * l);
                if fresh {
                    next = r0;
                } else if next < r0 {
                    if constant {
                        let n = ((r0 - next) / sp0).ceil();
                        idx = (idx + (n.rem_euclid(plen as f64)) as u64) % plen;
                        next += n * sp0;
                    } else {
                        next = r0;
                    }
                }
                while next <= r1 {
                    let t = if l > 0.0 {
                        ((next - acc) / l).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let p = Pt::lerp(a, b, t);
                    if !sample(p, pen, &mut idx) {
                        return false;
                    }
                    let n2 = next + c.spacing_at(p.s);
                    if n2.partial_cmp(&next) != Some(Ordering::Greater) {
                        // No longer advancing in floating point.
                        return false;
                    }
                    next = n2;
                }
                true
            });
            if !ok {
                return false;
            }
            acc += l;
        }
        true
    }

    /// The ranges one copy of a path of size `s` is drawn in (a segment is
    /// one, or ≤ |Δx| / period + 2 dashes; a fill's crossings double the
    /// segments) and its rows of travel; `rotated` as in [`Self::bound`].
    fn ranges(&self, s: &Stats, rotated: bool, f: &Frame) -> (f64, f64) {
        let c = self;
        let g = BoxG::of(f);
        // A rotated copy's rows and steps are each at most its grid length.
        let (dy, dx_steps) = if rotated {
            (s.len, s.len)
        } else {
            (s.dy, s.dx / g.step)
        };
        let segs = s.segs + if c.fill.is_some() { s.segs } else { 0.0 };
        let ranges = segs
            + match c.dash_grid(f) {
                Some(d) => dx_steps * g.step / d.period + 2.0 * segs,
                None => 0.0,
            };
        (ranges, dy)
    }

    /// The work drawing one copy of a path of size `s` costs when its brush
    /// changes along it, whatever it draws: the pieces its ranges are cut
    /// into (each at most [`MAX_PIECES`](super::draw::MAX_PIECES)) — for a
    /// band, times the rows each piece's quad scans; 0 for samples or a
    /// constant brush. Generators with many points check it against
    /// [`PIECE_CAP`] at build (V09-S60).
    pub fn pieces(&self, s: &Stats, rotated: bool, f: &Frame, clip: &Band) -> f64 {
        if !self.varies() || self.sampled() {
            return 0.0;
        }
        let (ranges, _) = self.ranges(s, rotated, f);
        let cut = (s.len / UNIT + ranges).min(MAX_PIECES as f64 * ranges);
        if self.banded() {
            let clip_rows = (clip.hi - clip.lo) as f64;
            let band_h = if self.fill.is_some() {
                clip_rows
            } else {
                self.thick.min(clip_rows)
            };
            cut * (band_h + 2.0)
        } else {
            cut
        }
    }

    /// The analytic bound of one copy of a path of size `s` (see
    /// `docs/GENERATORS.md` "Lines"); `rotated`: its rows and steps are
    /// each at most its grid length; `clip`: the pen's clip band.
    pub fn bound(&self, s: &Stats, rotated: bool, f: &Frame, clip: &Band) -> f64 {
        let c = self;
        let (ranges, dy) = c.ranges(s, rotated, f);
        // Pieces: one per grid unit when the brush varies (+ one per range).
        let pieces = ranges + if c.varies() { s.len + ranges } else { 0.0 };
        let clip_rows = (clip.hi - clip.lo) as f64;
        let band_h = if c.fill.is_some() {
            clip_rows
        } else {
            c.thick.min(clip_rows)
        };
        if c.sampled() {
            // Restarts (dashes, a spacing graph after undrawable rows) add
            // at most one sample per range.
            let samples = s.len / c.min_spacing() + ranges + s.lines + 1.0;
            let per = if c.banded() { band_h + 1.0 } else { 1.0 };
            return samples * per;
        }
        if c.banded() {
            // Each quad / bar: its rows of travel plus the band's height.
            return (dy + pieces * (band_h + 2.0)).min(pieces * (clip_rows + 1.0)) + s.lines;
        }
        dy + 2.0 * pieces + s.lines
    }

    /// A cheap lower bound of the notes one copy of `paths` (map `m`,
    /// clipped to the box when `clip`) makes, for the cases whose output
    /// grows with the path's length (samples, dashes): when it lies wholly
    /// in the drawable rows `rows` (and in the box when clipped to it),
    /// every sample (or every dash it fully crosses, halved) draws a note
    /// there — a dot pattern keeps its share, `keep` at least its least
    /// share; 0 otherwise.
    pub fn lower(
        &self,
        paths: &[Vec<Pt>],
        m: &Aff,
        clip: bool,
        g: &BoxG,
        rows: &Band,
        f: &Frame,
    ) -> f64 {
        let c = self;
        let dash = c.dash_grid(f);
        if !(c.sampled() || dash.is_some()) || (c.keep.is_some() && c.dots.is_some()) {
            return 0.0;
        }
        let least = c.least_kept();
        let mut total = 0.0;
        for line in paths {
            let (mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY);
            let (mut x0, mut x1) = (f64::INFINITY, f64::NEG_INFINITY);
            let mut len = 0.0;
            let mut prev: Option<Pt> = None;
            // (grid length so far, position) at each point, for `keep`.
            let mut along: Vec<(f64, f64)> = Vec::new();
            for &p in line {
                let q = m.ap(p);
                (y0, y1) = (y0.min(q.y), y1.max(q.y));
                (x0, x1) = (x0.min(q.x), x1.max(q.x));
                if let Some(o) = prev {
                    len += g.len(o, q);
                }
                if c.keep.is_some() {
                    along.push((len, q.s));
                }
                prev = Some(q);
            }
            if !(y0 >= rows.lo as f64 && y1 < rows.hi as f64) {
                return 0.0;
            }
            if clip && !(x0 >= g.x0 && x1 <= g.x1 && y0 >= g.lo && y1 <= g.hi) {
                return 0.0;
            }
            let dashes = dash
                .map_or(0.0, |d| ((x1 - x0) / d.period).floor() - 1.0)
                .max(0.0);
            let n = if c.sampled() && dash.is_none() {
                (len / c.max_spacing()).floor() + 1.0
            } else if c.sampled() {
                dashes
            } else {
                (dashes / 2.0).floor()
            };
            // The shares sum per path: of its samples ⌊Σ d⌋ are kept — at
            // least ⌊n · least⌋, or bin by bin.
            total += match &c.keep {
                Some(k) if c.sampled() => {
                    let binned = if dash.is_none() {
                        kept_in_bins(k, &along, len, c.max_spacing())
                    } else {
                        0.0
                    };
                    (n * least).floor().max(binned)
                }
                _ => n,
            };
        }
        if let Some(d) = &c.dots {
            let ones = d.iter().filter(|&&x| x).count() as f64;
            total = (total / d.len() as f64).floor() * ones;
        }
        total
    }
}

/// A lower bound of the samples `keep` keeps on a path of `len` grid units
/// (`along`: grid length so far and position `s` at each point), sampled at
/// most `widest` apart: in each of 64 equal stretches of it at least
/// ⌊stretch / widest⌋ samples, each adding at least the least share over
/// the stretch's positions — of the sum ⌊Σ⌋ are kept.
fn kept_in_bins(keep: &Curve, along: &[(f64, f64)], len: f64, widest: f64) -> f64 {
    const BINS: usize = 64;
    if along.len() < 2 || len.is_nan() || len <= 0.0 || widest.is_nan() || widest <= 0.0 {
        return 0.0;
    }
    let per = (len / BINS as f64 / widest).floor();
    if per < 1.0 || !per.is_finite() {
        return 0.0;
    }
    // The position at grid length `l` (linear between points).
    let s_at = |l: f64| -> f64 {
        let i = along.partition_point(|a| a.0 < l).clamp(1, along.len() - 1);
        let (a, b) = (along[i - 1], along[i]);
        if b.0 > a.0 {
            a.1 + (b.1 - a.1) * ((l - a.0) / (b.0 - a.0)).clamp(0.0, 1.0)
        } else {
            b.1
        }
    };
    let mut sum = 0.0;
    for k in 0..BINS {
        let (l0, l1) = (
            len * k as f64 / BINS as f64,
            len * (k + 1) as f64 / BINS as f64,
        );
        let (s0, s1) = (s_at(l0), s_at(l1));
        let (lo, hi) = (s0.min(s1), s0.max(s1));
        // Between the curve's points it is monotone: its least over [lo,
        // hi] is at an end or at a point inside.
        let mut m = keep.eval(lo).min(keep.eval(hi));
        for &(x, _) in &keep.points {
            if x > lo && x < hi {
                m = m.min(keep.eval(x));
            }
        }
        sum += m.clamp(0.0, 1.0) * per;
    }
    sum.floor()
}

/// Inserts the points where the path crosses `level` (so a band to that
/// level has corners there).
pub fn insert_crossings(pts: &mut Vec<Pt>, level: f64) {
    let mut i = 0;
    while i + 1 < pts.len() {
        let (a, b) = (pts[i], pts[i + 1]);
        let (da, db) = (a.y - level, b.y - level);
        if da * db < 0.0 {
            let t = da / (da - db);
            pts.insert(i + 1, Pt::lerp(a, b, t));
            i += 1;
        }
        i += 1;
    }
}

/// `t` ranges of segment `a → b` inside the box (row edges; Liang–Barsky).
pub fn clip_t(a: Pt, b: Pt, g: &BoxG) -> Option<(f64, f64)> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let (mut ta, mut tb) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, a.x - g.x0),
        (dx, g.x1 - a.x),
        (-dy, a.y - g.lo),
        (dy, g.hi - a.y),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let r = q / p;
        if p < 0.0 {
            ta = ta.max(r);
        } else {
            tb = tb.min(r);
        }
        if ta > tb {
            return None;
        }
    }
    Some((ta, tb))
}

/// Calls `f(t0, t1, fresh)` for the ranges of segment `a → b` that are
/// drawn, in order along it: inside `rows` (the pen's clip band — nothing
/// outside it is drawn), inside the box when `clip`, and in a dash —
/// `fresh` when the range begins at a dash's start. Dashes are visited
/// lazily, each one drawing at least one note, so a long segment costs
/// what it draws; stops (false) as soon as `f` does.
#[allow(clippy::too_many_arguments)]
pub fn for_ranges(
    a: Pt,
    b: Pt,
    g: &BoxG,
    clip: bool,
    rows: (f64, f64),
    dash: Option<Dash>,
    mut f: impl FnMut(f64, f64, bool) -> bool,
) -> bool {
    let (mut ta, mut tb) = (0.0f64, 1.0f64);
    if clip {
        match clip_t(a, b, g) {
            Some(r) => (ta, tb) = r,
            None => return true,
        }
    }
    let dy = b.y - a.y;
    if dy == 0.0 {
        if !(a.y >= rows.0 && a.y < rows.1) {
            return true;
        }
    } else {
        let (u, v) = ((rows.0 - a.y) / dy, (rows.1 - 1e-9 - a.y) / dy);
        ta = ta.max(u.min(v));
        tb = tb.min(u.max(v));
        if ta.is_nan() || tb.is_nan() || ta > tb {
            return true;
        }
    }
    let Some(d) = dash else {
        return f(ta, tb, false);
    };
    let dx = b.x - a.x;
    if dx == 0.0 {
        return !d.on_at(a.x) || f(ta, tb, false);
    }
    let (xa, xb) = (a.x + dx * ta, a.x + dx * tb);
    let (lo, hi) = (xa.min(xb), xa.max(xb));
    let m0 = ((lo - d.x0) / d.period).floor();
    let m1 = ((hi - d.x0) / d.period).floor();
    if !(m0.is_finite() && m1.is_finite()) {
        return true;
    }
    let forward = dx > 0.0;
    let mut last = f64::NAN;
    let mut i = 0.0f64;
    loop {
        let m = if forward { m0 + i } else { m1 - i };
        if (forward && m > m1) || (!forward && m < m0) {
            return true;
        }
        let ws = d.x0 + m * d.period;
        // Positions this far out no longer advance (or a dash no longer has
        // a width) in floating point: the rest is not drawn — the same in
        // both passes.
        if ws == last || (ws + d.on).partial_cmp(&ws) != Some(Ordering::Greater) {
            return true;
        }
        last = ws;
        let (w0, w1) = (ws.max(lo), (ws + d.on).min(hi));
        if w1 > w0 || (w1 == w0 && lo == hi) {
            let (u0, u1) = ((w0 - a.x) / dx, (w1 - a.x) / dx);
            let fresh = if forward { ws >= lo } else { ws + d.on <= hi };
            if !f(u0.min(u1).max(ta), u0.max(u1).min(tb), fresh) {
                return false;
            }
        }
        i += 1.0;
    }
}

/// Reused buffers of one drawing pass.
#[derive(Default)]
pub struct Scratch {
    pub line: Vec<Pt>,
    pub xy: Vec<(f64, f64)>,
    pub brushes: Vec<Brush>,
    pub ranges: Vec<(f64, f64)>,
    pub poly: Vec<(f64, f64)>,
    pub band: Vec<Pt>,
    /// Points a varying stroke holds before its part goes to the pen
    /// (`None`: [`STROKE_PART`]).
    pub part: Option<usize>,
}

/// Upper bounds of a materialized path's size (analytic: what a pipeline
/// can make at most, without building it).
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Points, segments, polylines.
    pub pts: f64,
    pub segs: f64,
    pub lines: f64,
    /// Σ |Δ rows|, Σ grid length, Σ |Δ ticks|.
    pub dy: f64,
    pub len: f64,
    pub dx: f64,
}

/// Most points a stroke with a varying brush holds before its part goes to
/// the pen ([`Draw::stroke_part`]; ~1.5 MB of points and brushes).
pub const STROKE_PART: usize = 1 << 16;

/// Most pieces one request's paths with a changing brush may be cut into
/// ([`PathStyle::pieces`]); beyond, [`too_many_pieces`] refuses it. V09-S60:
/// a Spirograph curve of 2²¹ points with a gate along it over a box spanning
/// every tick was cut into ~2·10⁹ pieces — 2 GB of points before the stroke
/// went in parts, then 277 s of counting; 2²⁴ pieces count in ~1 – 2 s.
pub const PIECE_CAP: f64 = (1u64 << 24) as f64;

/// The sentence refusing `n` pieces.
pub fn too_many_pieces(n: f64) -> TransformError {
    let say = |v: f64| {
        if v < 1e19 {
            group_digits(v as u64)
        } else {
            "more than 10 quintillion".into()
        }
    };
    TransformError::TooLarge(format!(
        "Changing the brush along the curve (its gate, velocity or subchannel) would cut it into {} pieces; at most {} are allowed — use a smaller box, a coarser resolution or fewer points.",
        say(n),
        say(PIECE_CAP)
    ))
}

/// Most samples one request's paths kept by shares may visit
/// ([`PathStyle::visits`]); beyond, [`too_many_visits`] refuses it.
pub const VISIT_CAP: f64 = (1u64 << 26) as f64;

/// The sentence refusing `n` visits.
pub fn too_many_visits(n: f64) -> TransformError {
    let say = |v: f64| {
        if v < 1e19 {
            group_digits(v as u64)
        } else {
            "more than 10 quintillion".into()
        }
    };
    TransformError::TooLarge(format!(
        "Keeping notes by their shares along the curve would visit {} grid units; at most {} are allowed — use a smaller box, a coarser resolution or a larger least share.",
        say(n),
        say(VISIT_CAP)
    ))
}

/// The exact size of a (small) polyline.
pub fn measure(pts: &[Pt], g: &BoxG) -> Stats {
    let mut s = Stats {
        pts: pts.len() as f64,
        segs: pts.len().saturating_sub(1) as f64,
        lines: 1.0,
        ..Stats::default()
    };
    for w in pts.windows(2) {
        s.dy += (w[1].y - w[0].y).abs();
        s.dx += (w[1].x - w[0].x).abs();
        s.len += g.len(w[0], w[1]);
    }
    s
}

// ----- curvature (lifted from Arcs, V09-S58: Parametric Equation shapes
// densities by it too)

/// The curvature (1 / grid units) at each point: the turn between its
/// segments over their mean length (the ends take their neighbour's).
pub fn curvature(p: &[Pt]) -> Vec<f64> {
    let n = p.len();
    let mut k = vec![0.0; n];
    for i in 1..n.saturating_sub(1) {
        let (a, b, c) = (p[i - 1], p[i], p[i + 1]);
        let (l0, l1) = ((b.x - a.x).hypot(b.y - a.y), (c.x - b.x).hypot(c.y - b.y));
        if l0 > 0.0 && l1 > 0.0 {
            let t0 = (b.y - a.y).atan2(b.x - a.x);
            let t1 = (c.y - b.y).atan2(c.x - b.x);
            let turn = (t1 - t0 + TAU / 2.0).rem_euclid(TAU) - TAU / 2.0;
            k[i] = turn.abs() / ((l0 + l1) / 2.0);
        }
    }
    if n >= 3 {
        k[0] = k[1];
        k[n - 1] = k[n - 2];
    }
    k
}

/// A curve of the position along `p`: `f(curvature)` at each point.
pub fn by_curvature(p: &[Pt], f: impl Fn(f64) -> f64) -> Curve {
    let k = curvature(p);
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(p.len());
    for (q, &kk) in p.iter().zip(&k) {
        let y = f(kk).clamp(0.0, 1.0);
        if pts.last().is_none_or(|l| q.s > l.0) {
            pts.push((q.s, y));
        }
    }
    if pts.len() < 2 {
        let y = pts.first().map_or(1.0, |p| p.1);
        pts = vec![(0.0, y), (1.0, y)];
    }
    Curve::new(pts)
}

/// A curve of the position along `pts`: `y(i)` at each point (lifted from
/// Parametric, V09-S60: the spirograph shapes its brushes the same way).
pub fn curve_along(pts: &[Pt], y: impl Fn(usize) -> f64) -> Curve {
    let mut c: Vec<(f64, f64)> = Vec::with_capacity(pts.len());
    for (i, q) in pts.iter().enumerate() {
        let v = y(i).clamp(0.0, 1.0);
        if c.last().is_none_or(|l| q.s > l.0) {
            c.push((q.s, v));
        }
    }
    if c.len() < 2 {
        let v = c.first().map_or(1.0, |p| p.1);
        c = vec![(0.0, v), (1.0, v)];
    }
    Curve::new(c)
}

#[cfg(test)]
mod tests {
    use super::super::pen::{Pen, Sink};
    use super::*;
    use crate::model::project::KeyRange;

    /// Records every note with its brush.
    #[derive(Default)]
    struct All(Vec<(i128, i128, i64, u8, u64, u32)>);

    impl Sink for All {
        fn note(&mut self, s: i128, d: i128, r: i64, b: &Brush) -> bool {
            self.0.push((s, d, r, b.vel, b.gate.to_bits(), b.sub));
            true
        }
    }

    /// A pen that counts the parts it is sent.
    struct Parts(Pen<All>, usize);

    impl Draw for Parts {
        fn stroke(&mut self, pts: &[(f64, f64)], brush: Brush) -> bool {
            self.0.stroke(pts, brush)
        }
        fn stroke_with(&mut self, pts: &[(f64, f64)], brushes: &[Brush]) -> bool {
            self.0.stroke_with(pts, brushes)
        }
        fn stroke_part(&mut self, pts: &[(f64, f64)], brushes: &[Brush]) -> bool {
            assert!(
                pts.len() == brushes.len() + 1,
                "a part's brushes are its segments'"
            );
            self.1 += 1;
            self.0.stroke_part(pts, brushes)
        }
        fn fill(&mut self, poly: &[(f64, f64)], brush: Brush) -> bool {
            self.0.fill(poly, brush)
        }
        fn note(&mut self, x0: f64, x1: f64, row: i64, brush: Brush) -> bool {
            self.0.note(x0, x1, row, brush)
        }
    }

    #[test]
    fn a_long_varying_stroke_reaches_the_pen_in_parts_with_the_same_notes() {
        let f = Frame {
            t0: 0,
            t1: 960 * 4_000,
            lo: 20,
            hi: 100,
            step: 30,
            ppq: 960,
            snap: 240,
            keys: KeyRange::Standard,
            seed: 0,
            brush: Brush {
                vel: 100,
                gate: 1.0,
                sub: 0,
            },
        };
        let g = BoxG::of(&f);
        let rows = f.band();
        // A zigzag of 30,000 points across the box (pieces of a grid unit
        // or two) with a gate and velocity gradient, dashed so the stroke
        // also breaks.
        let n = 30_000;
        let pts: Vec<Pt> = (0..n)
            .map(|i| {
                let s = i as f64 / (n - 1) as f64;
                let y = 20.0 + 80.0 * ((i as f64 * 0.37).sin() * 0.5 + 0.5);
                Pt {
                    x: s * (f.t1 as f64 - 1.0),
                    y,
                    s,
                }
            })
            .collect();
        for dash in [None, Some((960.0 * 7.0, 960.0))] {
            let mut st = PathStyle::default();
            st.grad.gate = Some((0.2, 1.0));
            st.grad.vel = Some((40.0, 127.0));
            st.dash = dash;
            assert!(st.varies());
            let draw = |part: Option<usize>| {
                let mut pen = Parts(Pen::new(All::default(), f.step, rows), 0);
                let mut sc = Scratch {
                    part,
                    ..Scratch::default()
                };
                assert!(st.draw_stroke(&pts, true, &g, &f, &rows, &mut pen, &mut sc));
                (pen.0.finish().0, pen.1)
            };
            let (whole, none) = draw(Some(usize::MAX));
            assert_eq!(none, 0);
            assert!(whole.len() > 10_000, "{}", whole.len());
            for part in [1, 2, 17, 1_000, STROKE_PART] {
                let (notes, parts) = draw(Some(part));
                assert_eq!(notes, whole, "parts of {part} points (dash {dash:?})");
                if part < 1_000 {
                    assert!(parts > 10, "{parts} parts of {part}");
                }
            }
        }
    }
}
