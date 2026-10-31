//! Flip's v0.9 kernel (V09-S39 step 6, `docs/TRANSFORM_ENGINE.md` "Flip"):
//! the functions declared by id beside the v0.8 kernel (`edit.rs`
//! `flip_build`, untouched — a module of its own so the v0.8 closure
//! compiles as before), with Control Sweeps per note.

use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::shared::fast_hash::FastMap;
use super::super::shared::metric_position::bar_bounds;
use super::super::shared::tempo_time::TempoTime;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::theory::Scale;
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceStats, TNote, TransformError};
use super::edit::sub_index;
use super::note_value;

pub(super) const FLIP_PLACE: &[&str] = &["After", "Before", "Overlay"];
pub(super) const FLIP_HALF: &[&str] = &["Upper", "Lower", "Left", "Right"];
pub(super) const FLIP_ANCHOR: &[&str] = &["Start", "End", "Center (v0.8)"];
pub(super) const FLIP_REGION: &[&str] = &["Ellipse", "Rectangle"];
pub(super) const FLIP_RECURSE: &[&str] = &["Time", "Pitch", "Alternate (time first)"];

/// Flip's v0.8 settings (the v0.8 kernel's closure reads the same values;
/// [`Flip9`] re-reads them here, unchanged).
#[derive(Clone, Debug)]
struct Flip8 {
    axis: usize,
    pivot: Option<(f64, i64)>,
    diagonal: bool,
    per_chord: bool,
    per_bar: bool,
    per_sub: bool,
    diatonic: Option<Scale>,
    gates: bool,
    vels: bool,
    alternate: bool,
    blend: Option<f64>,
    bar: u64,
}

/// a1's pivot list as (fraction of the span, pitch).
fn pivot_of(p: &Params) -> (f64, i64) {
    let l = p.list("a1.pivot");
    (
        l.first().copied().unwrap_or(500) as f64 / 1000.0,
        l.get(1).copied().unwrap_or(60),
    )
}

impl Flip8 {
    fn from(p: &Params, b: &BuildCtx) -> Flip8 {
        Flip8 {
            axis: p.choice("axis"),
            pivot: p.adv(1).then(|| pivot_of(p)),
            diagonal: p.adv(2),
            per_chord: p.adv(3),
            per_bar: p.adv(4),
            per_sub: p.adv(5),
            diatonic: p
                .adv(6)
                .then(|| Scale::new(p.choice("a6.key"), p.choice("a6.scale"))),
            gates: p.adv(7),
            vels: p.adv(8),
            alternate: p.adv(9),
            blend: p.adv(10).then(|| p.float("a10.amount")),
            bar: b.ctx.bar(),
        }
    }
}

/// Flip's settings of the functions declared by id (V09-S39 step 6), per
/// Control Sweep level ([`PerLevel`]).
#[derive(Clone, Debug, Default)]
pub(super) struct FlipNew {
    /// n01: placement (0 after, 1 before, 2 overlay), copy velocity %.
    append: Option<(usize, f64)>,
    /// n02: keep (0 upper, 1 lower, 2 left, 3 right), the pivot (a1's
    /// values) instead of the centre.
    half: Option<(usize, Option<(f64, i64)>)>,
    /// n03: anchor (0 start, 1 end, 2 centre), the grid when snapping.
    anchor: Option<(usize, Option<u64>)>,
    /// n04: period (ticks), pitch offset, copies.
    glide: Option<(u64, i64, u64)>,
    /// n05: angle (°), offset (% of the half box).
    angled: Option<(f64, f64)>,
    /// n06: in seconds (else ticks).
    real: Option<bool>,
    /// n07: subchannel position pairs (a, b, a, b, …).
    pairs: Option<Vec<i64>>,
    /// n08: kind (0 ellipse, 1 rectangle), centre x %, centre y %, size %.
    region: Option<(usize, f64, f64, f64)>,
    /// n09: depth, axis (0 time, 1 pitch, 2 alternate).
    recurse: Option<(u32, usize)>,
    /// n10: grid (ticks).
    swing: Option<u64>,
    /// n11: the axis path, range (rows).
    moving: Option<(Curve, i64)>,
}

impl FlipNew {
    pub(super) fn from(p: &Params, ctx: &Ctx) -> FlipNew {
        FlipNew {
            append: p
                .flag("n01")
                .then(|| (p.choice("n01.place"), p.float("n01.vel"))),
            half: p.flag("n02").then(|| {
                (
                    p.choice("n02.keep"),
                    (p.choice("n02.axis") == 1).then(|| pivot_of(p)),
                )
            }),
            anchor: p.flag("n03").then(|| {
                (
                    p.choice("n03.anchor"),
                    (p.choice("n03.snap") == 1).then(|| note_value(p.choice("n03.grid"), 0, ctx)),
                )
            }),
            glide: p.flag("n04").then(|| {
                (
                    p.ticks("n04.period").max(1),
                    p.int("n04.offset"),
                    p.int("n04.copies").clamp(1, 64) as u64,
                )
            }),
            angled: p
                .flag("n05")
                .then(|| (p.float("n05.angle"), p.float("n05.offset"))),
            real: p.flag("n06").then(|| p.choice("n06.in") == 0),
            pairs: p.flag("n07").then(|| p.list("n07.pairs")),
            region: p.flag("n08").then(|| {
                (
                    p.choice("n08.region"),
                    p.float("n08.cx"),
                    p.float("n08.cy"),
                    p.float("n08.size"),
                )
            }),
            recurse: p
                .flag("n09")
                .then(|| (p.int("n09.depth").clamp(1, 6) as u32, p.choice("n09.axis"))),
            swing: p
                .flag("n10")
                .then(|| note_value(p.choice("n10.grid"), 0, ctx).max(1)),
            moving: p
                .flag("n11")
                .then(|| (p.curve("n11.path"), p.int("n11.range").max(1))),
        }
    }

    pub(super) fn any(&self) -> bool {
        self.append.is_some()
            || self.half.is_some()
            || self.anchor.is_some()
            || self.glide.is_some()
            || self.angled.is_some()
            || self.real.is_some()
            || self.pairs.is_some()
            || self.region.is_some()
            || self.recurse.is_some()
            || self.swing.is_some()
            || self.moving.is_some()
    }
}

/// One output note in the making (48 bytes): its start, length, the
/// source note it comes from (its other fields), its subchannel, pitch and
/// velocity, whether it is a mirror image (n07 colours those; n01 copies
/// them) and whether it is dropped (n02's other half).
#[derive(Clone, Copy, Debug)]
struct Img {
    start: i128,
    dur: i128,
    i: u32,
    sub: u32,
    pitch: i16,
    vel: i16,
    flipped: bool,
    gone: bool,
}

/// A pitch or velocity in [`Img`]'s range (keys far outside 0 – 255 stay
/// outside, for the boundary policy).
fn narrow(v: i64) -> i16 {
    v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

impl Img {
    fn of(i: usize, x: &TNote) -> Img {
        Img {
            start: i128::from(x.start),
            dur: i128::from(x.dur),
            i: i as u32,
            sub: x.sub,
            pitch: i16::from(x.pitch),
            vel: i16::from(x.vel),
            flipped: false,
            gone: false,
        }
    }

    fn at(&self) -> usize {
        self.i as usize
    }

    /// Emits the image (the source note's other fields).
    fn emit(&self, notes: &[TNote], out: &mut Output) -> bool {
        let mut t = notes[self.at()];
        t.sub = self.sub;
        out.emit(
            self.start,
            self.dur,
            i64::from(self.pitch),
            i64::from(self.vel),
            &t,
        )
    }
}

/// A flipping set's box (ticks and keys) and its pivot (time, pitch).
#[derive(Clone, Copy, Debug)]
struct Box2 {
    s: i128,
    e: i128,
    lo: i64,
    hi: i64,
    px: f64,
    pp: f64,
    /// The pivot's place in the span (n06 measures it in seconds).
    fx: f64,
}

/// The v0.9 Flip kernel (V09-S39 step 6): the v0.8 rule re-read from the
/// same parameters, with the functions declared by id at their stages and
/// the Control Sweeps evaluated per note inside the kernel (n02's halves,
/// n08's region boxes and n10's pairs read across notes).
pub(super) struct Flip9 {
    v8: Flip8,
    new: FlipNew,
    rebuild: Option<std::sync::Arc<Rebuild>>,
    ctx: Ctx,
    stats: SourceStats,
    growth: Growth,
}

impl Flip9 {
    pub(super) fn build(
        p: &Params,
        b: &BuildCtx,
        new: FlipNew,
    ) -> Result<Box<dyn Kernel>, TransformError> {
        // Per note at most: itself, n02's mirror, n01's copy of both, n04's
        // copies.
        let per = if new.append.is_some() || new.glide.is_some() {
            1 + if new.append.is_some() { 2 } else { 0 } + new.glide.map_or(0, |g| g.2)
        } else if new.half.is_some() {
            2
        } else {
            1
        };
        Ok(Box::new(Flip9 {
            v8: Flip8::from(p, b),
            new,
            rebuild: Rebuild::of(p, b),
            ctx: b.ctx.clone(),
            stats: b.stats.clone(),
            growth: if per > 1 {
                Growth::Expand(per)
            } else {
                Growth::Map
            },
        }))
    }

    /// Whether note `x` lies in n08's region (`cfg`, % of the selection's
    /// box: its start and its key).
    fn inside(&self, x: &TNote, cfg: &FlipNew) -> bool {
        let Some((kind, cx, cy, size)) = cfg.region else {
            return true;
        };
        let st = &self.stats;
        let w = st.max_end.saturating_sub(st.min_start).max(1) as f64;
        let h = f64::from(st.max_pitch.saturating_sub(st.min_pitch)).max(1.0);
        let u = (x.start.saturating_sub(st.min_start) as f64) / w * 100.0;
        let v = (f64::from(x.pitch) - f64::from(st.min_pitch)) / h * 100.0;
        let half = (size / 2.0).max(1e-9);
        let (du, dv) = ((u - cx) / half, (v - cy) / half);
        if kind == 1 {
            du.abs() <= 1.0 && dv.abs() <= 1.0
        } else {
            du * du + dv * dv <= 1.0
        }
    }

    /// The v0.8 mirror of note `x` in box `b` (diagonal, time, pitch) with
    /// n03's anchor, n06's seconds, n05's angled line and n11's moving
    /// axis (a10's blend follows in the caller).
    fn mirror(&self, x: &TNote, b: &Box2, cfg: &FlipNew, tt: &mut TempoTime) -> (i128, i128, i64) {
        let v = &self.v8;
        let (s, e, lo, hi) = (b.s, b.e, b.lo, b.hi);
        let mut ns = i128::from(x.start);
        let mut nd = i128::from(x.dur);
        let mut np = i64::from(x.pitch);
        if v.diagonal {
            // v0.8: swap normalized time and pitch inside the box.
            let u = if e > s {
                (x.start as f64 - s as f64) / (e - s) as f64
            } else {
                0.0
            };
            let w = if hi > lo {
                (np - lo) as f64 / (hi - lo) as f64
            } else {
                0.0
            };
            ns = s + (w * (e - s) as f64).round() as i128;
            np = lo + (u * (hi - lo) as f64).round() as i64;
        } else if let Some((angle, offset)) = cfg.angled {
            // n05: reflect the onset point across the line in the box's
            // normalized space (both sides move).
            let w = (e - s).max(1) as f64;
            let h = (hi - lo).max(1) as f64;
            let u = (x.start as f64 - s as f64) / w;
            let q = (np - lo) as f64 / h;
            let (sn, cs) = angle.to_radians().sin_cos();
            let (nx, ny) = (-sn, cs);
            let (cx, cy) = (
                0.5 + 0.5 * offset / 100.0 * nx,
                0.5 + 0.5 * offset / 100.0 * ny,
            );
            let d = (u - cx) * nx + (q - cy) * ny;
            ns = (s as f64 + (u - 2.0 * d * nx) * w).round() as i128;
            np = (lo as f64 + (q - 2.0 * d * ny) * h).round() as i64;
        } else {
            if v.axis == 0 || v.axis == 2 {
                (ns, nd) = self.time_mirror(x, b, cfg, tt);
            }
            if v.axis == 1 || v.axis == 2 {
                np = match (&cfg.moving, &v.diatonic) {
                    (Some((path, range)), _) => {
                        // n11: the axis at the note's place in the span.
                        let u = if e > s {
                            ((x.start as f64 - s as f64) / (e - s) as f64).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let axis = b.pp + (path.eval(u) - 0.5) * *range as f64;
                        (2.0 * axis - np as f64).round() as i64
                    }
                    (None, _) => self.pitch_mirror(np, b.pp),
                };
            }
        }
        (ns, nd, np)
    }

    /// The v0.8 pitch mirror of `p` around `pp` (a6: by scale degrees).
    fn pitch_mirror(&self, p: i64, pp: f64) -> i64 {
        match &self.v8.diatonic {
            Some(sc) => {
                let (da, _) = sc.degree_of(pp.round() as i64);
                let (dp, off) = sc.degree_of(p);
                sc.pitch_of(2 * da - dp) - off
            }
            None => (2.0 * pp - p as f64).round() as i64,
        }
    }

    /// The time mirror of `x` around the box's pivot: v0.8's (the note's
    /// span reversed: its end becomes its start), or n03's anchor point
    /// (start, end or centre mirrored, optionally snapped to the grid), in
    /// ticks or (n06) in seconds through the tempo map.
    fn time_mirror(&self, x: &TNote, b: &Box2, cfg: &FlipNew, tt: &mut TempoTime) -> (i128, i128) {
        let (anchor, grid) = cfg.anchor.unwrap_or((2, None));
        let d = i128::from(x.dur);
        let snap = |t: f64| -> f64 {
            match grid {
                Some(g) if g > 0 => (t / g as f64).round() * g as f64,
                _ => t,
            }
        };
        if cfg.real == Some(true) {
            // In µs: the pivot at its place in the box's real-time span.
            let (s_us, e_us) = (
                tt.micros_f(b.s.max(0) as u64),
                tt.micros_f(b.e.max(0) as u64),
            );
            let px = s_us + (e_us - s_us) * b.fx;
            let (a, z) = (tt.micros_f(x.start), tt.micros_f(x.end()));
            let (ns_us, ne_us) = match anchor {
                0 => (2.0 * px - a, 2.0 * px - a + (z - a)),
                1 => (2.0 * px - z - (z - a), 2.0 * px - z),
                _ => (2.0 * px - z, 2.0 * px - a),
            };
            let mut tick = |us: f64| -> i128 {
                if us < 0.0 {
                    -i128::from(tt.tick_at_micros_f(-us))
                } else {
                    i128::from(tt.tick_at_micros_f(us))
                }
            };
            let (mut ns, mut ne) = (tick(ns_us), tick(ne_us));
            if grid.is_some() {
                let len = ne - ns;
                match anchor {
                    0 => {
                        ns = snap(ns as f64) as i128;
                        ne = ns + len;
                    }
                    1 => {
                        ne = snap(ne as f64) as i128;
                        ns = ne - len;
                    }
                    _ => {
                        let c = snap((ns + ne) as f64 / 2.0);
                        ns = (c - len as f64 / 2.0).round() as i128;
                        ne = ns + len;
                    }
                }
            }
            return (ns, (ne - ns).max(1));
        }
        let px = b.px;
        let ns = match anchor {
            0 => snap(2.0 * px - x.start as f64).round() as i128,
            1 => snap(2.0 * px - x.end() as f64).round() as i128 - d,
            _ => match grid {
                Some(_) => {
                    let c = x.start as f64 + x.dur as f64 / 2.0;
                    (snap(2.0 * px - c) - x.dur as f64 / 2.0).round() as i128
                }
                // v0.8: the span reversed.
                None => (2.0 * px - x.end() as f64).round() as i128,
            },
        };
        (ns, d)
    }

    /// n09: the note through `depth` levels of mirrors — the box, then the
    /// half (along the level's axis) the note lands in, and so on — time
    /// levels reverse the note's span within the box, pitch levels mirror
    /// its key.
    fn recurse(x: &TNote, b: &Box2, depth: u32, axis: usize) -> (i128, i64) {
        let (mut bs, mut be) = (b.s as f64, b.e as f64);
        let (mut bl, mut bh) = (b.lo as f64, b.hi as f64);
        let mut st = x.start as f64;
        let d = x.dur as f64;
        let mut p = f64::from(x.pitch);
        for level in 0..depth {
            let time = match axis {
                0 => true,
                1 => false,
                _ => level % 2 == 0,
            };
            if time {
                st = bs + be - (st + d);
                let mid = (bs + be) / 2.0;
                if st + d / 2.0 < mid {
                    be = mid;
                } else {
                    bs = mid;
                }
            } else {
                p = bl + bh - p;
                let mid = (bl + bh) / 2.0;
                if p < mid {
                    bh = mid;
                } else {
                    bl = mid;
                }
            }
        }
        (st.round() as i128, p.round() as i64)
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let notes = &src.notes;
        let n = notes.len();
        if n == 0 {
            return Ok(());
        }
        let lv: PerLevel<FlipNew> = match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, |p, b| FlipNew::from(p, b.ctx)),
            None => PerLevel::one(self.new.clone()),
        };
        let any = |f: fn(&FlipNew) -> bool| lv.all().iter().any(f);
        let v = &self.v8;
        let mut tt = TempoTime::new(&self.ctx);
        // ---- Groups (v0.8: per bar / per subchannel / alternate bars).
        let key = |x: &TNote| -> (u64, u32) {
            let b0 = if v.per_bar || v.alternate {
                x.start / v.bar.max(1)
            } else {
                0
            };
            (b0, if v.per_sub { x.sub } else { u32::MAX })
        };
        let mut groups: std::collections::BTreeMap<(u64, u32), Vec<usize>> = Default::default();
        for (i, x) in notes.iter().enumerate() {
            groups.entry(key(x)).or_default().push(i);
        }
        let mut imgs: Vec<Img> = notes
            .iter()
            .enumerate()
            .map(|(i, x)| Img::of(i, x))
            .collect();
        // n02's mirrors.
        let mut extra: Vec<Img> = Vec::new();
        // Each note's flipping set (`u32::MAX`: none) and the sets' boxes
        // (n01 shifts by their spans, n04 mirrors around their pivots).
        let copies = any(|c| c.append.is_some()) || any(|c| c.glide.is_some());
        let mut set_of: Vec<u32> = if copies {
            vec![u32::MAX; n]
        } else {
            Vec::new()
        };
        let mut boxes: Vec<Box2> = Vec::new();
        let region = any(|c| c.region.is_some());
        let mut outside = 0usize;
        for ((barno, _), idx) in &groups {
            if v.alternate && barno % 2 == 0 {
                continue;
            }
            // ---- n08: which notes flip (the region), in their own box.
            let set: std::borrow::Cow<[usize]> = if region {
                std::borrow::Cow::Owned(
                    idx.iter()
                        .copied()
                        .filter(|&i| self.inside(&notes[i], lv.at(i)))
                        .collect(),
                )
            } else {
                std::borrow::Cow::Borrowed(idx)
            };
            outside += idx.len() - set.len();
            if set.is_empty() {
                continue;
            }
            let s = set.iter().map(|&i| notes[i].start).min().unwrap_or(0) as i128;
            let e = set.iter().map(|&i| notes[i].end()).max().unwrap_or(0) as i128;
            let lo = set.iter().map(|&i| notes[i].pitch).min().unwrap_or(0) as i64;
            let hi = set.iter().map(|&i| notes[i].pitch).max().unwrap_or(0) as i64;
            let (fx, px, pp) = match v.pivot {
                Some((fx, pitch)) => (fx, s as f64 + (e - s) as f64 * fx, pitch as f64),
                None => (0.5, (s + e) as f64 / 2.0, (lo + hi) as f64 / 2.0),
            };
            let bx = Box2 {
                s,
                e,
                lo,
                hi,
                px,
                pp,
                fx,
            };
            if copies {
                for &i in set.iter() {
                    set_of[i] = boxes.len() as u32;
                }
            }
            boxes.push(bx);
            if v.gates || v.vels {
                // v0.8: lengths / velocities in reverse order, nothing moves.
                let mut order = set.to_vec();
                order.sort_by_key(|&i| (notes[i].start, notes[i].pitch));
                if v.gates {
                    let durs: Vec<u64> = order.iter().rev().map(|&i| notes[i].dur).collect();
                    for (k, &i) in order.iter().enumerate() {
                        imgs[i].dur = i128::from(durs[k]);
                    }
                }
                if v.vels {
                    let vs: Vec<u8> = order.iter().rev().map(|&i| notes[i].vel).collect();
                    for (k, &i) in order.iter().enumerate() {
                        imgs[i].vel = i16::from(vs[k]);
                    }
                }
                for &i in set.iter() {
                    imgs[i].flipped = true;
                }
                continue;
            }
            if let Some((keep, pivot)) = lv.at(set[0]).half {
                // ---- n02: one half kept, the other replaced by its mirror.
                let (hpx, hpp) = match pivot {
                    Some((fx, pitch)) => (s as f64 + (e - s) as f64 * fx, pitch as f64),
                    None => (px, pp),
                };
                for &i in set.iter() {
                    let x = &notes[i];
                    let c = x.start as f64 + x.dur as f64 / 2.0;
                    let p = f64::from(x.pitch);
                    let kept = match keep {
                        0 => p >= hpp,
                        1 => p <= hpp,
                        2 => c <= hpx,
                        _ => c >= hpx,
                    };
                    if !kept {
                        imgs[i].gone = true;
                        continue;
                    }
                    let mut m = Img::of(i, x);
                    if keep < 2 {
                        m.pitch = narrow(self.pitch_mirror(i64::from(x.pitch), hpp));
                    } else {
                        m.start = (2.0 * hpx - x.end() as f64).round() as i128;
                    }
                    if m.pitch != i16::from(x.pitch) || m.start != i128::from(x.start) {
                        m.flipped = true;
                        extra.push(m);
                    }
                }
                continue;
            }
            for &i in set.iter() {
                let x = &notes[i];
                let cfg = lv.at(i);
                let (mut ns, nd, mut np) = match (cfg.recurse, v.diagonal) {
                    // ---- n09: halves inside halves.
                    (Some((depth, axis)), false) => {
                        let (ns, np) = Self::recurse(x, &bx, depth, axis);
                        (ns, i128::from(x.dur), np)
                    }
                    _ => self.mirror(x, &bx, cfg, &mut tt),
                };
                if let Some(a) = v.blend {
                    // v0.8's a10.
                    ns = (x.start as f64 + (ns as f64 - x.start as f64) * a).round() as i128;
                    np = (f64::from(x.pitch) + (np as f64 - f64::from(x.pitch)) * a).round() as i64;
                }
                let m = &mut imgs[i];
                m.start = ns;
                m.dur = nd;
                m.pitch = narrow(np);
                m.flipped = true;
            }
            out.check()?;
        }
        // v0.8's a3: the pitch order reversed inside each onset group (one
        // image per note: not with n02).
        if v.per_chord && !any(|c| c.half.is_some()) {
            let mut i = 0;
            while i < n {
                let mut j = i;
                while j < n && notes[j].start == notes[i].start && notes[j].track == notes[i].track
                {
                    j += 1;
                }
                let mut ps: Vec<i16> = (i..j).map(|k| imgs[k].pitch).collect();
                ps.sort_unstable();
                let mut order: Vec<usize> = (i..j).collect();
                order.sort_by_key(|&k| imgs[k].pitch);
                for (r, &k) in order.iter().enumerate() {
                    imgs[k].pitch = ps[ps.len() - 1 - r];
                }
                i = j;
            }
        }
        // ---- n10: the timing inside every grid pair mirrored (the pair's
        // first inner onset m → length − m; the beats stay).
        if any(|c| c.swing.is_some()) {
            self.swing(notes, &mut imgs, &mut extra, &lv);
        }
        // ---- n07: the mirror images' subchannels by pairs.
        if any(|c| c.pairs.is_some()) {
            for m in imgs.iter_mut().chain(extra.iter_mut()) {
                if m.flipped && !m.gone {
                    m.sub = recolour(src, lv.at(m.at()), &notes[m.at()], m.sub);
                }
            }
        }
        // ---- Output: the images (every kept note's, then n02's mirrors),
        // or (n01 / n04) the selection kept with the mirror images as copies.
        let images = || imgs.iter().chain(extra.iter()).filter(|m| !m.gone);
        // V09-0143: the copy (0 = the note or its flipped image, 1 = n01's
        // appended mirror, k = n04's glide copy k).
        out.set_dim(0);
        if copies {
            for x in notes {
                if !out.push(*x) {
                    return out.check();
                }
            }
            out.set_dim(1);
            for m in images().filter(|m| m.flipped) {
                let cfg = lv.at(m.at());
                let Some((place, pct)) = cfg.append else {
                    continue;
                };
                let Some(b) = boxes.get(set_of[m.at()] as usize) else {
                    continue;
                };
                let shift = match place {
                    0 => b.e - b.s,
                    1 => b.s - b.e,
                    _ => 0,
                };
                let mut c = *m;
                c.start += shift;
                c.vel = narrow((f64::from(m.vel) * pct / 100.0).round() as i64);
                if !c.emit(notes, out) {
                    return out.check();
                }
            }
            // n04: glide copies of every note in a flipping set.
            for (i, x) in notes.iter().enumerate() {
                let cfg = lv.at(i);
                let Some((period, offset, count)) = cfg.glide else {
                    continue;
                };
                let Some(b) = boxes.get(set_of[i] as usize) else {
                    continue;
                };
                for k in 1..=count {
                    out.set_dim(u32::try_from(k).unwrap_or(u32::MAX));
                    let mut c = Img::of(i, x);
                    c.start += i128::from(k) * i128::from(period) / 2;
                    if k % 2 == 1 {
                        // A mirror image: n07 colours it too.
                        c.pitch = narrow(self.pitch_mirror(i64::from(x.pitch), b.pp) + offset);
                        c.sub = recolour(src, cfg, x, c.sub);
                    }
                    if !c.emit(notes, out) {
                        return out.check();
                    }
                }
                out.check()?;
            }
        } else {
            for m in images() {
                if !m.emit(notes, out) {
                    return out.check();
                }
            }
        }
        if region && boxes.is_empty() {
            out.message = Some("Region flip: no note lies inside the region.".into());
        } else if region && outside > 0 {
            out.message = Some(format!(
                "Region flip: {} note(s) inside the region flipped.",
                n - outside
            ));
        }
        out.check()
    }

    /// n10 on the images: per track, each grid pair `[P, P + 2g)` from its
    /// bar's start; its split m (the first image start inside the pair after
    /// P) maps to `2g − m` piecewise linearly, P and P + 2g fixed; starts in
    /// the pair and ends inside it move.
    fn swing(&self, notes: &[TNote], imgs: &mut [Img], extra: &mut [Img], lv: &PerLevel<FlipNew>) {
        // (track, grid, pair start) → the first inner onset (offset).
        let pair_of = |t: i128, g: u64| -> Option<(u64, u64)> {
            let t = u64::try_from(t).ok()?;
            let (bar_start, _) = bar_bounds(&self.ctx.meter, t);
            let l = 2 * g;
            let p = bar_start + (t - bar_start) / l * l;
            Some((p, t - p))
        };
        let mut split: FastMap<(u16, u64, u64), u64> = FastMap::default();
        for m in imgs.iter().chain(extra.iter()) {
            if !m.flipped || m.gone {
                continue;
            }
            let Some(g) = lv.at(m.at()).swing else {
                continue;
            };
            if let Some((p, o)) = pair_of(m.start, g)
                && o > 0
            {
                let e = split.entry((notes[m.at()].track, g, p)).or_insert(o);
                *e = (*e).min(o);
            }
        }
        for m in imgs.iter_mut().chain(extra.iter_mut()) {
            if !m.flipped || m.gone {
                continue;
            }
            let Some(g) = lv.at(m.at()).swing else {
                continue;
            };
            let l = (2 * g) as f64;
            let Some((p, o)) = pair_of(m.start, g) else {
                continue;
            };
            let Some(&mid) = split.get(&(notes[m.at()].track, g, p)) else {
                continue;
            };
            let mid = mid as f64;
            let f = |o: f64| -> f64 {
                if o <= mid {
                    o * (l - mid) / mid
                } else {
                    (l - mid) + (o - mid) * mid / (l - mid)
                }
            };
            let end = m.start + m.dur;
            let ns = i128::from(p) + f(o as f64).round() as i128;
            let ne = if end <= i128::from(p) + (2 * g) as i128 {
                i128::from(p) + f((end - i128::from(p)) as f64).round() as i128
            } else {
                end
            };
            m.start = ns;
            m.dur = (ne - ns).max(1);
        }
    }
}

/// n07: subchannel `sub` of an image of `x` swapped with its pair's
/// (positions in the track's table) when `cfg` names one.
fn recolour(src: &Source, cfg: &FlipNew, x: &TNote, sub: u32) -> u32 {
    let Some(pairs) = cfg.pairs.as_ref().filter(|p| p.len() >= 2) else {
        return sub;
    };
    let Some(tr) = src.track(x.track).filter(|t| !t.subs.is_empty()) else {
        return sub;
    };
    let pos = sub_index(src, &TNote { sub, ..*x }) as i64;
    let swap = pairs.chunks_exact(2).find_map(|w| {
        if w[0] == pos {
            Some(w[1])
        } else if w[1] == pos {
            Some(w[0])
        } else {
            None
        }
    });
    match swap {
        Some(to) => tr.subs[(to.max(0) as usize) % tr.subs.len()].0,
        None => sub,
    }
}

impl Kernel for Flip9 {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        self.run_all(src, out)
    }
}
