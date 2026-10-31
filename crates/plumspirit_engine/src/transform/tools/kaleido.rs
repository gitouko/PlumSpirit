//! Kaleidoscope's v0.9 kernel (V09-S40 step 4, `docs/TRANSFORM_ENGINE.md`
//! "Kaleidoscope"): the functions declared by id beside the v0.8 kernel
//! (`xgeom.rs` `kaleido_build`, untouched — a module of its own so the v0.8
//! closure compiles as before), with Control Sweeps per note.
//!
//! A note's copies come from n01's wallpaper group when it is on (the
//! group's operations in every cell of a wall), else from rings of sectors —
//! n06's rings, or v0.8's one ring of a1's sectors — around the selection's
//! centre or, with n11, around the centre of the note's chord, bar or group
//! (v0.8's mirrored and pinwheel sectors, a4 / a5 mirror images, a8's radial
//! scaling, a9's nested levels). n02 spins the whole system across time, n08
//! breaks N sectors by a seeded amount, n03 delays and n05 transposes sector
//! k, n10 fills only the sectors of a pattern, n07 mirrors in musical units
//! (the centre on a bar line or beat and a key note; every copy's pitch in
//! the key); n04 thins the copies to the original's note count and n09
//! merges the doubled notes along the mirror seams.

use super::super::geom::Geom;
use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::shared::metric_position::{bar_bounds, metric_position};
use super::super::shared::scale_lattice::Lattice;
use super::super::shared::seeded_stream::{salt_of, stream};
use super::super::sweep::{PerLevel, Rebuild};
use super::super::theory::Scale;
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::xgeom::{mid_unit, rot, sub_stepped};
use std::f64::consts::PI;
use std::sync::Arc;

/// The farthest a copy is placed (unit coordinates of the box).
const FAR: f64 = 1e12;

/// The switches of the v0.9 functions.
pub(super) const NEW: [&str; 11] = [
    "n01", "n02", "n03", "n04", "n05", "n06", "n07", "n08", "n09", "n10", "n11",
];

/// n01: the 17 plane symmetry groups (international notation).
pub(super) const GROUPS: &[&str] = &[
    "p1", "p2", "pm", "pg", "cm", "pmm", "pmg", "pgg", "cmm", "p4", "p4m", "p4g", "p3", "p3m1",
    "p31m", "p6", "p6m",
];
pub(super) const THIN: &[&str] = &["Interleave", "Outline", "None"];
pub(super) const MIRROR_AT: &[&str] = &["Bar lines", "Beats"];
pub(super) const SEAM_KEEP: &[&str] = &["One", "One with blended velocity"];
pub(super) const UNITS: &[&str] = &["Chord", "Bar", "Group"];

/// A general position of a wallpaper group in fractional coordinates:
/// (X, Y) → (a·X + b·Y + ½·hx, c·X + d·Y + ½·hy).
type Op = (i8, i8, i8, i8, u8, u8);

const I: Op = (1, 0, 0, 1, 0, 0);

/// n01: each group's general positions (International Tables, plane groups;
/// the hexagonal ones in hexagonal coordinates) and whether its lattice is
/// hexagonal.
fn group_ops(g: usize) -> (&'static [Op], bool) {
    const P2: &[Op] = &[I, (-1, 0, 0, -1, 0, 0)];
    const PM: &[Op] = &[I, (-1, 0, 0, 1, 0, 0)];
    const PG: &[Op] = &[I, (-1, 0, 0, 1, 0, 1)];
    const CM: &[Op] = &[
        I,
        (-1, 0, 0, 1, 0, 0),
        (1, 0, 0, 1, 1, 1),
        (-1, 0, 0, 1, 1, 1),
    ];
    const PMM: &[Op] = &[
        I,
        (-1, 0, 0, -1, 0, 0),
        (-1, 0, 0, 1, 0, 0),
        (1, 0, 0, -1, 0, 0),
    ];
    const PMG: &[Op] = &[
        I,
        (-1, 0, 0, -1, 0, 0),
        (-1, 0, 0, 1, 1, 0),
        (1, 0, 0, -1, 1, 0),
    ];
    const PGG: &[Op] = &[
        I,
        (-1, 0, 0, -1, 0, 0),
        (-1, 0, 0, 1, 1, 1),
        (1, 0, 0, -1, 1, 1),
    ];
    const CMM: &[Op] = &[
        I,
        (-1, 0, 0, -1, 0, 0),
        (-1, 0, 0, 1, 0, 0),
        (1, 0, 0, -1, 0, 0),
        (1, 0, 0, 1, 1, 1),
        (-1, 0, 0, -1, 1, 1),
        (-1, 0, 0, 1, 1, 1),
        (1, 0, 0, -1, 1, 1),
    ];
    const P4: &[Op] = &[
        I,
        (0, -1, 1, 0, 0, 0),
        (-1, 0, 0, -1, 0, 0),
        (0, 1, -1, 0, 0, 0),
    ];
    const P4M: &[Op] = &[
        I,
        (0, -1, 1, 0, 0, 0),
        (-1, 0, 0, -1, 0, 0),
        (0, 1, -1, 0, 0, 0),
        (-1, 0, 0, 1, 0, 0),
        (0, 1, 1, 0, 0, 0),
        (1, 0, 0, -1, 0, 0),
        (0, -1, -1, 0, 0, 0),
    ];
    const P4G: &[Op] = &[
        I,
        (0, -1, 1, 0, 0, 0),
        (-1, 0, 0, -1, 0, 0),
        (0, 1, -1, 0, 0, 0),
        (-1, 0, 0, 1, 1, 1),
        (1, 0, 0, -1, 1, 1),
        (0, 1, 1, 0, 1, 1),
        (0, -1, -1, 0, 1, 1),
    ];
    const P3: &[Op] = &[I, (0, -1, 1, -1, 0, 0), (-1, 1, -1, 0, 0, 0)];
    const P3M1: &[Op] = &[
        I,
        (0, -1, 1, -1, 0, 0),
        (-1, 1, -1, 0, 0, 0),
        (0, -1, -1, 0, 0, 0),
        (-1, 1, 0, 1, 0, 0),
        (1, 0, 1, -1, 0, 0),
    ];
    const P31M: &[Op] = &[
        I,
        (0, -1, 1, -1, 0, 0),
        (-1, 1, -1, 0, 0, 0),
        (0, 1, 1, 0, 0, 0),
        (1, -1, 0, -1, 0, 0),
        (-1, 0, -1, 1, 0, 0),
    ];
    const P6: &[Op] = &[
        I,
        (0, -1, 1, -1, 0, 0),
        (-1, 1, -1, 0, 0, 0),
        (-1, 0, 0, -1, 0, 0),
        (0, 1, -1, 1, 0, 0),
        (1, -1, 1, 0, 0, 0),
    ];
    const P6M: &[Op] = &[
        I,
        (0, -1, 1, -1, 0, 0),
        (-1, 1, -1, 0, 0, 0),
        (-1, 0, 0, -1, 0, 0),
        (0, 1, -1, 1, 0, 0),
        (1, -1, 1, 0, 0, 0),
        (0, -1, -1, 0, 0, 0),
        (-1, 1, 0, 1, 0, 0),
        (1, 0, 1, -1, 0, 0),
        (0, 1, 1, 0, 0, 0),
        (1, -1, 0, -1, 0, 0),
        (-1, 0, -1, 1, 0, 0),
    ];
    match g {
        1 => (P2, false),
        2 => (PM, false),
        3 => (PG, false),
        4 => (CM, false),
        5 => (PMM, false),
        6 => (PMG, false),
        7 => (PGG, false),
        8 => (CMM, false),
        9 => (P4, false),
        10 => (P4M, false),
        11 => (P4G, false),
        12 => (P3, true),
        13 => (P3M1, true),
        14 => (P31M, true),
        15 => (P6, true),
        16 => (P6M, true),
        _ => (&[I], false),
    }
}

/// n08: per ring, per sector: the extra turn and scale (0, 1 unbroken).
type Broken = Vec<Vec<(f64, f64)>>;

/// n01: a wall of cells (unit coordinates of the selection's box).
#[derive(Clone, Copy, Debug)]
struct Wall {
    ops: &'static [Op],
    hex: bool,
    cw: f64,
    ch: f64,
    rt: u32,
    rp: u32,
}

/// One note's settings (the v0.8 rule's and the functions'), per Control
/// Sweep level.
#[derive(Clone, Debug)]
struct KCfg {
    // v0.8 (`kaleido_build`).
    n_sec: usize,
    mirror: bool,
    twist: f64,
    pm: usize,
    tm: usize,
    sub_step: i64,
    cx: f64,
    cy: f64,
    rscale: f64,
    levels: usize,
    lscale: f64,
    wedge: bool,
    // v0.9.
    wall: Option<Wall>,
    /// n02: turns across the selection's time.
    spin: f64,
    /// n03: ticks per sector.
    delay: f64,
    /// n04: 0 interleave, 1 outline.
    thin: Option<usize>,
    /// n05: semitones per sector.
    transpose: Arc<Vec<i64>>,
    /// The rings: sectors of each (n06's list, else one ring of a1's / n11's
    /// count) and n06's spacing (unit).
    rings: Arc<Vec<usize>>,
    spacing: f64,
    /// n07: the key's lattice, mirror at beats (else bar lines).
    diatonic: Option<(Lattice, bool)>,
    /// n08: per ring, per sector: the broken sectors' (turn, scale).
    broken: Option<Arc<Broken>>,
    /// n09: seam width (ticks), blend velocities.
    seam: Option<(u64, bool)>,
    /// n10: the sectors filled (empty = all).
    pattern: Arc<Vec<bool>>,
    /// n11: unit (0 chord, 1 bar, 2 group).
    local: Option<usize>,
}

impl KCfg {
    fn from(p: &Params, b: &BuildCtx) -> KCfg {
        let w = b.stats.max_end.saturating_sub(b.stats.min_start).max(1) as f64;
        let rows = f64::from(b.stats.max_pitch.saturating_sub(b.stats.min_pitch)) + 1.0;
        let n_sec = if p.flag("n11") {
            p.int("n11.sectors").clamp(2, 32) as usize
        } else if p.adv(1) {
            p.int("a1.n").clamp(2, 24) as usize
        } else {
            2
        };
        let rings: Vec<usize> = if p.flag("n06") {
            let list = p.list("n06.sectors");
            let count = p.int("n06.rings").clamp(1, 8) as usize;
            (0..count)
                .map(|r| {
                    if list.is_empty() {
                        n_sec
                    } else {
                        list[r % list.len()].clamp(1, 64) as usize
                    }
                })
                .collect()
        } else {
            vec![n_sec]
        };
        let broken = p.flag("n08").then(|| {
            let count = p.int("n08.count").max(0) as usize;
            let amount = (p.float("n08.amount") / 100.0).clamp(0.0, 1.0);
            let seed = p.seed("n08.seed");
            let salt = salt_of("kaleidoscope.n08");
            Arc::new(
                rings
                    .iter()
                    .enumerate()
                    .map(|(r, &ns)| {
                        // The ring's `count` sectors of the smallest seeded
                        // keys are broken.
                        let mut keyed: Vec<(u64, usize)> = (0..ns)
                            .map(|s| (stream(seed, salt, &[r as u64, s as u64, 0]).next_u64(), s))
                            .collect();
                        keyed.sort_unstable();
                        let mut v = vec![(0.0, 1.0); ns];
                        for &(_, s) in keyed.iter().take(count) {
                            let mut rng = stream(seed, salt, &[r as u64, s as u64, 1]);
                            let turn = (rng.unit() - 0.5) * amount * (2.0 * PI / ns as f64);
                            let scale = 1.0 + (rng.unit() - 0.5) * amount * 0.5;
                            v[s] = (turn, scale);
                        }
                        v
                    })
                    .collect(),
            )
        });
        KCfg {
            n_sec,
            mirror: p.adv(2),
            twist: if p.adv(3) {
                p.float("a3.twist") * PI / 180.0
            } else {
                0.0
            },
            pm: if p.adv(4) { 2 } else { 1 },
            tm: if p.adv(5) { 2 } else { 1 },
            sub_step: if p.adv(6) { p.int("a6.step") } else { 0 },
            cx: if p.adv(7) {
                p.float("a7.x") / 100.0
            } else {
                0.5
            },
            cy: if p.adv(7) {
                p.float("a7.y") / 100.0
            } else {
                0.5
            },
            rscale: if p.adv(8) {
                p.float("a8.scale") / 100.0
            } else {
                1.0
            },
            levels: if p.adv(9) {
                p.int("a9.levels").clamp(2, 4) as usize
            } else {
                1
            },
            lscale: if p.adv(9) {
                p.float("a9.scale") / 100.0
            } else {
                1.0
            },
            wedge: p.adv(10),
            wall: p.flag("n01").then(|| {
                let (ops, hex) = group_ops(p.choice("n01.group"));
                Wall {
                    ops,
                    hex,
                    cw: p.ticks("n01.width").max(1) as f64 / w,
                    ch: p.int("n01.height").clamp(1, 64) as f64 / rows,
                    rt: p.int("n01.rt").clamp(1, 64) as u32,
                    rp: p.int("n01.rp").clamp(1, 16) as u32,
                }
            }),
            spin: if p.flag("n02") {
                p.float("n02.turns")
            } else {
                0.0
            },
            delay: if p.flag("n03") {
                p.ticks("n03.delay") as f64
            } else {
                0.0
            },
            thin: p
                .flag("n04")
                .then(|| p.choice("n04.mode"))
                .filter(|&m| m < 2),
            transpose: Arc::new(if p.flag("n05") {
                p.list("n05.semis")
            } else {
                Vec::new()
            }),
            spacing: if p.flag("n06") {
                p.float("n06.spacing") / 100.0
            } else {
                0.0
            },
            rings: Arc::new(rings),
            diatonic: p.flag("n07").then(|| {
                (
                    Lattice::of_scale(&Scale::new(p.choice("n07.key"), p.choice("n07.scale"))),
                    p.choice("n07.at") == 1,
                )
            }),
            broken,
            seam: p
                .flag("n09")
                .then(|| (p.ticks("n09.width"), p.choice("n09.keep") == 1)),
            pattern: Arc::new(if p.flag("n10") {
                p.list("n10.pattern").iter().map(|&x| x != 0).collect()
            } else {
                Vec::new()
            }),
            local: p.flag("n11").then(|| p.choice("n11.unit").min(2)),
        }
    }

    /// The copies of a note before n10 / n04 (n01's ops × cells, else the
    /// rings' sectors × a4 × a5 × a9).
    fn copies(&self) -> usize {
        match self.wall {
            Some(w) => w.ops.len() * w.rt as usize * w.rp as usize,
            None => self.rings.iter().sum::<usize>() * self.pm * self.tm * self.levels,
        }
    }
}

/// One copy of a note: its sector (index in its ring, or n01's operation),
/// ring, a4 / a5 mirror images, a9 level, n01's cell.
#[derive(Clone, Copy, Debug, Default)]
struct Copy {
    sec: usize,
    ring: usize,
    ns: usize,
    pmir: bool,
    tmir: bool,
    level: usize,
    cell: (u32, u32),
}

/// A note's frame: the centre and the box (unit coordinates of the
/// selection's box) it is mirrored in — the selection's, or n11's unit's.
#[derive(Clone, Copy, Debug)]
struct Frame {
    u0: f64,
    v0: f64,
    wu: f64,
    hv: f64,
}

const WHOLE: Frame = Frame {
    u0: 0.0,
    v0: 0.0,
    wu: 1.0,
    hv: 1.0,
};

pub(super) struct Kal9 {
    base: KCfg,
    rebuild: Option<Arc<Rebuild>>,
    g: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    growth: Growth,
}

pub(super) fn kal9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let base = KCfg::from(p, b);
    let rebuild = Rebuild::of(p, b);
    let corners = match &rebuild {
        Some(r) => r.corners(b.ctx, KCfg::from),
        None => vec![base.clone()],
    };
    // The bound per note over every swept corner: each copy rasterized.
    let per = corners
        .iter()
        .map(|c| (c.copies() as u64).max(1))
        .max()
        .unwrap_or(1)
        .saturating_mul(g.per_note());
    let k = Kal9 {
        base,
        rebuild,
        g,
        ctx: b.ctx.clone(),
        tracks: b.tracks.to_vec(),
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
    };
    Ok(Box::new(k))
}

impl Kal9 {
    fn levels(&self, src: &Source) -> PerLevel<KCfg> {
        match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, KCfg::from),
            None => PerLevel::one(self.base.clone()),
        }
    }

    /// n07: (u, v) of the centre on the bar line / beat and the key note
    /// nearest it.
    fn musical_centre(&self, c: &KCfg, cx: f64, cy: f64) -> (f64, f64) {
        let Some((lat, beats)) = c.diatonic else {
            return (cx, cy);
        };
        let b = &self.g.bbox;
        let (x, y) = b.from_unit(cx, cy);
        let tick = x.round().clamp(0.0, u64::MAX as f64) as u64;
        let (start, len) = if beats {
            let m = metric_position(&self.ctx.meter, tick);
            (m.beat_start, m.beat_len)
        } else {
            bar_bounds(&self.ctx.meter, tick)
        };
        let line = if tick - start >= len / 2 {
            start.saturating_add(len)
        } else {
            start
        };
        let row = lat.nearest(y.floor() as i64) as f64 + 0.5;
        b.to_unit(line as f64, row)
    }

    /// n11: each note's unit frame (its chord, bar or group's box).
    fn frames(&self, src: &Source, unit: usize) -> Vec<Frame> {
        let notes = &src.notes;
        let n = notes.len();
        // The unit key of every note, then each key's box.
        let key: Vec<u64> = match unit {
            0 => notes
                .iter()
                .map(|x| u64::from(x.track) << 48 ^ x.start)
                .collect(),
            1 => notes
                .iter()
                .map(|x| bar_bounds(&self.ctx.meter, x.start).0)
                .collect(),
            _ => notes
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    if x.group == 0 {
                        // An ungrouped note is its own unit.
                        1u64 << 63 | i as u64
                    } else {
                        u64::from(x.group)
                    }
                })
                .collect(),
        };
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_unstable_by_key(|&i| (key[i], i));
        let b = &self.g.bbox;
        let mut out = vec![WHOLE; n];
        let mut i = 0;
        while i < n {
            let mut j = i;
            let (mut t0, mut t1, mut p0, mut p1) = (u64::MAX, 0u64, u8::MAX, 0u8);
            while j < n && key[idx[j]] == key[idx[i]] {
                let x = &notes[idx[j]];
                t0 = t0.min(x.start);
                t1 = t1.max(x.end());
                p0 = p0.min(x.pitch);
                p1 = p1.max(x.pitch);
                j += 1;
            }
            if unit == 1 {
                let (bs, bl) = bar_bounds(&self.ctx.meter, notes[idx[i]].start);
                t0 = t0.min(bs);
                t1 = t1.max(bs.saturating_add(bl));
            }
            let (u0, v0) = b.to_unit(t0 as f64, f64::from(p0));
            let (u1, v1) = b.to_unit(t1.max(t0 + 1) as f64, f64::from(p1) + 1.0);
            let f = Frame {
                u0,
                v0,
                wu: (u1 - u0).max(1e-9),
                hv: (v1 - v0).max(1e-9),
            };
            for &k in &idx[i..j] {
                out[k] = f;
            }
            i = j;
        }
        out
    }

    /// n04 outline: the top and the bottom note of each onset of a track.
    fn outline(src: &Source) -> Vec<bool> {
        let notes = &src.notes;
        let mut idx: Vec<usize> = (0..notes.len()).collect();
        idx.sort_unstable_by_key(|&i| (notes[i].track, notes[i].start, notes[i].pitch, i));
        let mut keep = vec![false; notes.len()];
        let mut i = 0;
        while i < idx.len() {
            let mut j = i;
            let (t, s) = (notes[idx[i]].track, notes[idx[i]].start);
            while j < idx.len() && notes[idx[j]].track == t && notes[idx[j]].start == s {
                j += 1;
            }
            keep[idx[i]] = true;
            keep[idx[j - 1]] = true;
            i = j;
        }
        keep
    }

    /// Every copy of a note with settings `c`, in v0.8's order (sector
    /// fastest, then a4, a5, a9) — n01's operations fastest, then cells.
    fn each_copy(c: &KCfg, mut f: impl FnMut(usize, Copy) -> bool) -> bool {
        let mut k = 0;
        if let Some(w) = c.wall {
            for r in 0..w.rp {
                for t in 0..w.rt {
                    for s in 0..w.ops.len() {
                        if !f(
                            k,
                            Copy {
                                sec: s,
                                ns: w.ops.len(),
                                cell: (t, r),
                                ..Copy::default()
                            },
                        ) {
                            return false;
                        }
                        k += 1;
                    }
                }
            }
            return true;
        }
        for level in 0..c.levels {
            for t in 0..c.tm {
                for p in 0..c.pm {
                    for (ring, &ns) in c.rings.iter().enumerate() {
                        for s in 0..ns {
                            if !f(
                                k,
                                Copy {
                                    sec: s,
                                    ring,
                                    ns,
                                    pmir: p == 1,
                                    tmir: t == 1,
                                    level,
                                    cell: (0, 0),
                                },
                            ) {
                                return false;
                            }
                            k += 1;
                        }
                    }
                }
            }
        }
        true
    }

    /// v0.8's map of copy `k` (`kaleido_build`) with the functions: the
    /// point (u, v) of a note whose middle is at `un` (time, in its frame).
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn map(
        &self,
        c: &KCfg,
        k: &Copy,
        fr: &Frame,
        centre: (f64, f64),
        un: f64,
        u: f64,
        v: f64,
    ) -> (f64, f64) {
        if let Some(w) = c.wall {
            let (a, b, cc, d, hx, hy) = w.ops[k.sec];
            let (x, y) = (u - 0.5, v - 0.5);
            let x2 = f64::from(a) * x + f64::from(b) * y + 0.5 * f64::from(hx);
            let y2 = f64::from(cc) * x + f64::from(d) * y + 0.5 * f64::from(hy);
            let fx = f64::from(k.cell.0) + 0.5 + x2;
            let fy = f64::from(k.cell.1) + 0.5 + y2;
            let (mut uu, vv) = (fx * w.cw, fy * w.ch);
            if w.hex {
                uu -= fy * w.cw * 0.5;
            }
            return self.finish(c, k, uu, vv);
        }
        let (cx, cy) = centre;
        let (mut x, mut y) = if fr.wu == 1.0 && fr.hv == 1.0 && fr.u0 == 0.0 && fr.v0 == 0.0 {
            (u - cx, v - cy)
        } else {
            ((u - cx) / fr.wu, (v - cy) / fr.hv)
        };
        let sec = k.sec;
        if c.mirror && sec % 2 == 1 {
            y = -y;
        }
        let mut ang = 2.0 * PI * sec as f64 / k.ns as f64 + c.twist * sec as f64;
        if c.spin != 0.0 {
            ang += 2.0 * PI * c.spin * un;
        }
        let mut s = c.rscale.powi(sec as i32) * c.lscale.powi(k.level as i32);
        if let Some(b) = &c.broken
            && let Some(&(turn, scale)) = b.get(k.ring).and_then(|r| r.get(sec))
        {
            ang += turn;
            s *= scale;
        }
        (x, y) = rot(x, y, ang);
        x *= s;
        y *= s;
        if k.pmir {
            y = -y;
        }
        if k.tmir {
            x = -x;
        }
        if c.spacing != 0.0 && k.ring > 0 {
            // n06: ring r pushed out by r × the spacing along its sector.
            let a = 2.0 * PI * sec as f64 / k.ns as f64 + PI / 4.0;
            let r = k.ring as f64 * c.spacing;
            x += r * a.cos();
            y += r * a.sin();
        }
        let (uu, vv) = if fr.wu == 1.0 && fr.hv == 1.0 && fr.u0 == 0.0 && fr.v0 == 0.0 {
            (cx + x, cy + y)
        } else {
            (cx + x * fr.wu, cy + y * fr.hv)
        };
        self.finish(c, k, uu, vv)
    }

    /// n03's delay, n05's transposition and n07's key of copy `k`.
    #[inline]
    fn finish(&self, c: &KCfg, k: &Copy, mut u: f64, mut v: f64) -> (f64, f64) {
        let b = &self.g.bbox;
        if c.delay != 0.0 {
            u += c.delay * k.sec as f64 / b.width();
        }
        if !c.transpose.is_empty() {
            v += c.transpose[k.sec % c.transpose.len()] as f64 / b.rows();
        }
        if let Some((lat, _)) = c.diatonic {
            let (_, y) = b.from_unit(u, v);
            if y.is_finite() && y.abs() < 1e9 {
                let row = lat.nearest(y.floor() as i64) as f64 + 0.5;
                v = b.to_unit(0.0, row).1;
            }
        }
        // Beyond ±10¹² boxes nothing lands on the keyboard or the tick
        // range: bounded, so the rasterizer's row arithmetic stays in i64.
        (u.clamp(-FAR, FAR), v.clamp(-FAR, FAR))
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let lv = self.levels(src);
        let start = out.notes.len();
        let any = |f: fn(&KCfg) -> bool| lv.all().iter().any(f);
        let frames = lv
            .all()
            .iter()
            .find_map(|c| c.local)
            .map(|u| self.frames(src, u));
        let outline = if any(|c| c.thin == Some(1)) {
            Self::outline(src)
        } else {
            Vec::new()
        };
        let g = &self.g;
        for (i, n) in src.notes.iter().enumerate() {
            let c = lv.at(i);
            let fr = frames
                .as_ref()
                .filter(|_| c.local.is_some())
                .map_or(WHOLE, |f| f[i]);
            let centre = {
                let (cx, cy) = (fr.u0 + c.cx * fr.wu, fr.v0 + c.cy * fr.hv);
                self.musical_centre(c, cx, cy)
            };
            let (nu, nv) = mid_unit(g, n);
            let un = ((nu - fr.u0) / fr.wu).clamp(0.0, 1.0);
            if c.wedge && c.wall.is_none() {
                let a = ((nv - centre.1) / fr.hv)
                    .atan2((nu - centre.0) / fr.wu)
                    .rem_euclid(2.0 * PI);
                if a >= 2.0 * PI / c.n_sec as f64 {
                    continue;
                }
            }
            // n10's filled sectors, n04's share of the copies.
            let active = if c.pattern.is_empty() {
                c.copies()
            } else {
                let mut m = 0;
                Self::each_copy(c, |_, k| {
                    m += usize::from(c.pattern[k.sec % c.pattern.len()]);
                    true
                });
                m
            };
            if c.thin == Some(1) && !outline[i] {
                continue;
            }
            let mut seen = 0usize;
            let ok = Self::each_copy(c, |_, k| {
                if !c.pattern.is_empty() && !c.pattern[k.sec % c.pattern.len()] {
                    return true;
                }
                let me = seen;
                seen += 1;
                if c.thin == Some(0) && active > 0 && i % active != me {
                    return true;
                }
                let t = sub_stepped(&self.tracks, n, c.sub_step * k.sec as i64);
                // V09-0143: the sector (0 = the original's).
                out.set_dim(u32::try_from(k.sec).unwrap_or(u32::MAX));
                g.rasterize(
                    &t,
                    &|u, v| self.map(c, &k, &fr, centre, un, u, v),
                    &self.tracks,
                    out,
                )
            });
            if !ok {
                return out.check();
            }
            out.check()?;
        }
        if let Some((width, blend)) = lv.all().iter().find_map(|c| c.seam) {
            seams(&mut out.notes, start, width, blend);
        }
        Ok(())
    }
}

impl Kernel for Kal9 {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        self.run_all(src, out)?;
        out.check()
    }
}

/// n09: the notes from `start` on that double each other — the same track,
/// pitch and subchannel, starts within `width` ticks (the copies meeting on
/// a mirror seam) — merged into the first (by start), its velocity the
/// cluster's mean when `blend`. O(n log n).
fn seams(notes: &mut Vec<TNote>, start: usize, width: u64, blend: bool) {
    let tail = &notes[start.min(notes.len())..];
    let m = tail.len();
    let mut idx: Vec<u32> = (0..m as u32).collect();
    idx.sort_unstable_by_key(|&i| {
        let x = &tail[i as usize];
        (x.track, x.pitch, x.sub, x.start, i)
    });
    let mut keep = vec![true; m];
    let mut vel: Vec<Option<u8>> = vec![None; m];
    let mut i = 0;
    while i < m {
        let a = &tail[idx[i] as usize];
        let mut j = i + 1;
        let mut sum = u64::from(a.vel);
        while j < m {
            let b = &tail[idx[j] as usize];
            if (b.track, b.pitch, b.sub) != (a.track, a.pitch, a.sub)
                || b.start.saturating_sub(a.start) > width
            {
                break;
            }
            keep[idx[j] as usize] = false;
            sum += u64::from(b.vel);
            j += 1;
        }
        if blend && j - i > 1 {
            vel[idx[i] as usize] = Some((sum / (j - i) as u64).clamp(1, 127) as u8);
        }
        i = j;
    }
    let base = start.min(notes.len());
    let mut w = base;
    for r in 0..m {
        if keep[r] {
            let mut x = notes[base + r];
            if let Some(v) = vel[r] {
                x.vel = v;
            }
            notes[w] = x;
            w += 1;
        }
    }
    notes.truncate(w);
}
