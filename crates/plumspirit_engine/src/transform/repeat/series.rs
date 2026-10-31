//! The series of a Paste Repeat (v0.9 V09-S14 / S15, `docs/PASTE_REPEAT.md`):
//! what both adapters (notes, clips) do to the core's offsets before they
//! place anything — once per run, never per note or per clip. The plan's
//! offsets become a **copy list** (`CopySlot`: generation, offset, gap);
//! the acceleration graph rescales the gaps, a morph adds to them, the
//! seeded draw omits or rearranges whole generations and the tree adds
//! child blocks. Moved here from the notes tool in S15 so the clip adapter
//! uses the same mathematics.

use super::super::param::Curve;
use super::super::rng::{Rng, hash, salt};
use crate::midi::group_digits;

/// One copy of the selection: built once per run from the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopySlot {
    /// The generation, 1 … N: what the per-copy functions read.
    pub k: u32,
    /// Added to every source note's start.
    pub off: u64,
    /// The distance to the previous copy (the first: its offset).
    pub gap: u64,
}

/// A copy's place in the series, 0 – 1 (the x of every per-copy graph).
pub fn place(k: u32, n: usize) -> f64 {
    if n <= 1 {
        0.0
    } else {
        f64::from(k.saturating_sub(1)) / (n - 1) as f64
    }
}

/// The acceleration graph's factor for a y of 0 – 1: 0 → 0 %, ½ → 100 %,
/// 1 → `max` (as a factor).
pub fn spacing_factor(y: f64, max: f64) -> f64 {
    let y = y.clamp(0.0, 1.0);
    if y <= 0.5 {
        2.0 * y
    } else {
        1.0 + (y - 0.5) * 2.0 * (max - 1.0)
    }
}

/// A value running over the series: from `start` to `end`, straight or by
/// a graph (y 0 = start, 1 = end).
#[derive(Clone, Debug)]
pub struct Morph {
    pub start: f64,
    pub end: f64,
    pub curve: Option<Curve>,
}

impl Morph {
    pub fn at(&self, u: f64) -> f64 {
        let t = match &self.curve {
            Some(c) => c.eval(u).clamp(0.0, 1.0),
            None => u,
        };
        self.start + (self.end - self.start) * t
    }
}

/// The pitch shift of generation `k`: `k × step`, or with a sequence the
/// first `k` intervals added up (cycled: `Σ seq[(i − 1) mod len]`, in whole
/// cycles, saturating).
pub fn cumulative(step: i64, seq: &[i64], k: u32) -> i64 {
    let kk = i64::from(k);
    if seq.is_empty() {
        return step.saturating_mul(kk);
    }
    let len = seq.len() as i64;
    let cycle: i64 = seq.iter().fold(0i64, |a, &b| a.saturating_add(b));
    let rest: i64 = seq
        .iter()
        .take((kk % len) as usize)
        .fold(0i64, |a, &b| a.saturating_add(b));
    (kk / len).saturating_mul(cycle).saturating_add(rest)
}

/// The copy list of plan offsets `offsets` (strictly increasing), with the
/// gaps rescaled by the acceleration graph (the notes' function 3) when
/// given. A gap that comes to 0 ticks is refused with a sentence (the S13
/// rule: never a copy on the previous one's tick).
pub fn copies(offsets: &[u64], accel: Option<(&Curve, f64)>) -> Result<Vec<CopySlot>, String> {
    copies_with(offsets, accel, None)
}

/// [`copies`] with a morph added to every gap (the clips' function 3):
/// gap *k* = gap × acceleration + `span × m(u)` (`m` a fraction: negative
/// closes the copies up until they overlap).
pub fn copies_with(
    offsets: &[u64],
    accel: Option<(&Curve, f64)>,
    morph: Option<(&Morph, u64)>,
) -> Result<Vec<CopySlot>, String> {
    let n = offsets.len();
    let mut out = Vec::with_capacity(n);
    let (mut prev, mut at) = (0u64, 0u64);
    let too_long = || "The spacing makes the copies too long for the song.".to_string();
    for (i, &o) in offsets.iter().enumerate() {
        let k = (i + 1) as u32;
        let base = o - prev;
        prev = o;
        let gap = if accel.is_none() && morph.is_none() {
            base
        } else {
            let u = place(k, n);
            let mut g = match accel {
                None => base as f64,
                Some((c, max)) => base as f64 * spacing_factor(c.eval(u), max),
            };
            if let Some((m, span)) = morph {
                g += span as f64 * m.at(u);
            }
            let g = g.round();
            if g < 1.0 {
                return Err(match (accel.is_some(), morph.is_some()) {
                    (true, false) => format!(
                        "The acceleration graph puts copy {k} on the previous one (its spacing would be under one tick); raise the graph there."
                    ),
                    _ => format!(
                        "The spacing puts copy {k} on or before the previous one; lower the overlap or raise the graph there."
                    ),
                });
            }
            if g >= u64::MAX as f64 {
                return Err(too_long());
            }
            g as u64
        };
        at = at.checked_add(gap).ok_or_else(too_long)?;
        out.push(CopySlot { k, off: at, gap });
    }
    Ok(out)
}

/// Whether generation `k` of `n` is picked by the seeded draw of kind
/// `salt` under the chance graph (whole generations, deterministic).
pub fn picked(seed: u64, salt: u64, k: u32, n: usize, chance: &Curve) -> bool {
    Rng::new(hash(&[seed, salt, u64::from(k)])).unit() < chance.eval(place(k, n)).clamp(0.0, 1.0)
}

/// The notes' function 7 (the clips' 9 omits with it): which copies stay
/// and which generation each slot plays. `omit` / `rearrange` per the mode;
/// a copy is picked when its draw (seed, salt, k) falls under the graph's
/// chance at its place.
pub fn fracture(list: &mut Vec<CopySlot>, chance: &Curve, seed: u64, omit: bool, rearrange: bool) {
    let n = list.len();
    if omit {
        list.retain(|c| !picked(seed, salt::KEEP, c.k, n, chance));
    }
    if rearrange {
        let at: Vec<usize> = (0..list.len())
            .filter(|&i| picked(seed, salt::ORDER, list[i].k, n, chance))
            .collect();
        let mut gens: Vec<u32> = at.iter().map(|&i| list[i].k).collect();
        // Fisher–Yates with one stream for the whole series.
        let mut r = Rng::new(hash(&[seed, salt::ORDER, at.len() as u64]));
        for i in (1..gens.len()).rev() {
            let j = (r.next_u64() % (i as u64 + 1)) as usize;
            gens.swap(i, j);
        }
        for (&i, g) in at.iter().zip(gens) {
            list[i].k = g;
        }
    }
}

/// Blocks per first-level copy of a tree `depth` deep with `children` per
/// block: `Σ_{d=0..depth} children^d` (saturating).
pub fn tree_blocks(depth: u32, children: u32) -> u64 {
    (0..=depth)
        .map(|d| u64::from(children).saturating_pow(d))
        .fold(0u64, u64::saturating_add)
}

/// The tree (the notes' function 10, the clips' 10): every block spawns
/// `children` copies `ratio` of its gap apart, down to `depth` levels; a
/// child plays its block's generation. The total is counted (and refused
/// above `limit`) before anything is built.
pub fn tree(
    list: &[CopySlot],
    depth: u32,
    children: u32,
    ratio: f64,
    limit: u64,
) -> Result<Vec<CopySlot>, String> {
    let total = (list.len() as u64).saturating_mul(tree_blocks(depth, children));
    if total > limit {
        return Err(format!(
            "The repeat tree would make {} copies (the limit is {}); lower its depth or its copies per block.",
            group_digits(total),
            group_digits(limit)
        ));
    }
    let mut out = Vec::with_capacity(total as usize);
    let mut level: Vec<CopySlot> = list.to_vec();
    out.extend_from_slice(list);
    for _ in 0..depth {
        let mut next = Vec::with_capacity(level.len() * children as usize);
        for c in &level {
            let g = (c.gap as f64 * ratio).round();
            if g < 1.0 {
                return Err(
                    "The repeat tree's copies would come less than one tick apart; raise the child spacing or lower the depth."
                        .into(),
                );
            }
            let g = g as u64;
            for j in 1..=u64::from(children) {
                let off = g
                    .checked_mul(j)
                    .and_then(|d| c.off.checked_add(d))
                    .ok_or_else(|| {
                        "The repeat tree reaches past the end of the song.".to_string()
                    })?;
                next.push(CopySlot {
                    k: c.k,
                    off,
                    gap: g,
                });
            }
        }
        out.extend_from_slice(&next);
        level = next;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: u64 = 1_000_000;

    #[test]
    fn the_copy_list_keeps_the_plan_and_the_acceleration_graph_rescales_its_gaps() {
        let plan = [100, 200, 300, 400, 500];
        let plain = copies(&plan, None).unwrap();
        assert_eq!(
            plain
                .iter()
                .map(|c| (c.k, c.off, c.gap))
                .collect::<Vec<_>>(),
            vec![
                (1, 100, 100),
                (2, 200, 100),
                (3, 300, 100),
                (4, 400, 100),
                (5, 500, 100)
            ]
        );
        // The flat default line (y ½) changes nothing.
        let flat = Curve::new(vec![(0.0, 0.5), (1.0, 0.5)]);
        assert_eq!(copies(&plan, Some((&flat, 2.0))).unwrap(), plain);
        // Accelerating: the gaps shrink from 200 % to 0 % … refused at 0.
        let down = Curve::new(vec![(0.0, 1.0), (1.0, 0.0)]);
        let e = copies(&plan, Some((&down, 2.0))).unwrap_err();
        assert!(e.contains("copy 5"), "{e}");
        // 200 % → 25 %: gaps 200, 156 (1.5625), 113, 69 (0.6875), 25.
        let down = Curve::new(vec![(0.0, 1.0), (1.0, 0.125)]);
        let c = copies(&plan, Some((&down, 2.0))).unwrap();
        let gaps: Vec<u64> = c.iter().map(|c| c.gap).collect();
        assert_eq!(gaps, vec![200, 156, 113, 69, 25]);
        assert_eq!(c.last().unwrap().off, gaps.iter().sum::<u64>());
        // Uneven plans (real time under a tempo change): each own gap scaled.
        let uneven = [960, 1920, 2400, 2880];
        let up = Curve::new(vec![(0.0, 0.5), (1.0, 1.0)]);
        let c = copies(&uneven, Some((&up, 3.0))).unwrap();
        // y = ½, ⅔, ⅚, 1 → factors 1, 5/3, 7/3, 3 (top = 300 %).
        assert_eq!(
            c.iter().map(|c| c.gap).collect::<Vec<_>>(),
            vec![960, 1600, 1120, 1440]
        );
        assert_eq!(place(1, 1), 0.0);
        assert_eq!(place(3, 5), 0.5);
    }

    #[test]
    fn a_morph_adds_a_share_of_the_span_to_every_gap_and_refuses_a_backward_copy() {
        // End to end: offsets every 1,000 (the span); the morph 0 → −50 %.
        let plan = [1000, 2000, 3000, 4000, 5000];
        let m = Morph {
            start: 0.0,
            end: -0.5,
            curve: None,
        };
        let c = copies_with(&plan, None, Some((&m, 1000))).unwrap();
        // u = 0, ¼, ½, ¾, 1 → gaps 1000, 875, 750, 625, 500.
        assert_eq!(
            c.iter().map(|c| c.gap).collect::<Vec<_>>(),
            vec![1000, 875, 750, 625, 500]
        );
        assert_eq!(c.last().unwrap().off, 3750);
        // With the acceleration too: gap × factor + span × m.
        let up = Curve::new(vec![(0.0, 1.0), (1.0, 1.0)]);
        let c = copies_with(&plan, Some((&up, 2.0)), Some((&m, 1000))).unwrap();
        assert_eq!(
            c.iter().map(|c| c.gap).collect::<Vec<_>>(),
            vec![2000, 1875, 1750, 1625, 1500]
        );
        // −100 % puts the last copy on the previous one: a sentence.
        let m = Morph {
            start: 0.0,
            end: -1.0,
            curve: None,
        };
        let e = copies_with(&plan, None, Some((&m, 1000))).unwrap_err();
        assert!(e.contains("copy 5"), "{e}");
        // A flat zero morph is the plain list.
        let z = Morph {
            start: 0.0,
            end: 0.0,
            curve: None,
        };
        assert_eq!(
            copies_with(&plan, None, Some((&z, 1000))).unwrap(),
            copies(&plan, None).unwrap()
        );
    }

    #[test]
    fn fracture_omits_and_rearranges_whole_copies_by_seed() {
        let base = copies(&(1..=40).map(|k| k * 100).collect::<Vec<u64>>(), None).unwrap();
        let half = Curve::new(vec![(0.0, 0.5), (1.0, 0.5)]);
        let mut a = base.clone();
        fracture(&mut a, &half, 7, true, false);
        let mut b = base.clone();
        fracture(&mut b, &half, 7, true, false);
        assert_eq!(a, b, "deterministic");
        assert!(a.len() > 5 && a.len() < 35, "{}", a.len());
        assert!(
            a.iter().all(|c| base.contains(c)),
            "kept copies are unchanged"
        );
        let mut c = base.clone();
        fracture(&mut c, &half, 8, true, false);
        assert_ne!(a, c, "another seed picks other copies");
        // Never / always.
        let mut none = base.clone();
        fracture(
            &mut none,
            &Curve::new(vec![(0.0, 0.0), (1.0, 0.0)]),
            7,
            true,
            true,
        );
        assert_eq!(none, base);
        let mut all = base.clone();
        fracture(
            &mut all,
            &Curve::new(vec![(0.0, 1.0), (1.0, 1.0)]),
            7,
            true,
            false,
        );
        assert!(all.is_empty());
        // Rearranging keeps every slot's time and permutes the generations.
        let mut r = base.clone();
        fracture(
            &mut r,
            &Curve::new(vec![(0.0, 1.0), (1.0, 1.0)]),
            3,
            false,
            true,
        );
        assert_eq!(
            r.iter().map(|c| c.off).collect::<Vec<_>>(),
            base.iter().map(|c| c.off).collect::<Vec<_>>()
        );
        let mut ks: Vec<u32> = r.iter().map(|c| c.k).collect();
        assert_ne!(ks, (1..=40).collect::<Vec<u32>>());
        ks.sort_unstable();
        assert_eq!(ks, (1..=40).collect::<Vec<u32>>());
    }

    #[test]
    fn the_tree_counts_and_places_its_blocks_exactly() {
        let base = copies(&[1000, 2000], None).unwrap();
        let t = tree(&base, 2, 2, 0.5, LIMIT).unwrap();
        // Per block 1 + 2 + 4 = 7.
        assert_eq!(tree_blocks(2, 2), 7);
        assert_eq!(t.len(), 14);
        let offs: Vec<(u32, u64)> = t.iter().map(|c| (c.k, c.off)).collect();
        for want in [
            (1, 1000),
            (1, 1500),
            (1, 2000),
            (1, 1750),
            (1, 2000),
            (1, 2250),
            (1, 2500),
            (2, 2500),
            (2, 3000),
            (2, 3250),
            (2, 3500),
        ] {
            assert!(offs.contains(&want), "{want:?} in {offs:?}");
        }
        let e = tree(&copies(&[10], None).unwrap(), 4, 2, 0.05, LIMIT).unwrap_err();
        assert!(e.contains("less than one tick"), "{e}");
        let many = copies(&(1..=1000).map(|k| k * 10_000).collect::<Vec<u64>>(), None).unwrap();
        let e = tree(&many, 4, 16, 0.5, LIMIT).unwrap_err();
        assert!(e.contains("69,905,000 copies"), "{e}");
    }
}
