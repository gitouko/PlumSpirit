//! `euclid`: Bjorklund (Euclidean) rhythms — `pulses` onsets spread as
//! evenly as possible over `steps`, rotated — for step gates, slice rhythms
//! and onset lattices (Toussaint, "The Euclidean algorithm generates
//! traditional musical rhythms", 2005: E(3,8) = x..x..x., E(5,8) =
//! x.xx.xx.). The pattern is built once per setting; asking whether a step
//! is an onset is O(1).

/// Most steps a pattern may have (tool controls stay within it).
pub const MAX_STEPS: u32 = 256;

/// A rotated Euclidean pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Euclid {
    hits: Vec<bool>,
    /// Rotation to the left, `0 .. steps`.
    rotation: usize,
}

impl Euclid {
    /// `pulses` onsets over `steps` (`steps` clamped to 1 ..= [`MAX_STEPS`],
    /// `pulses` to `0 ..= steps`), rotated left by `rotation` steps (step
    /// `i` of the result is step `i + rotation` of the unrotated pattern;
    /// negative values rotate right).
    pub fn new(steps: u32, pulses: u32, rotation: i64) -> Euclid {
        let steps = steps.clamp(1, MAX_STEPS) as usize;
        let pulses = (pulses as usize).min(steps);
        Euclid {
            hits: bjorklund(steps, pulses),
            rotation: rotation.rem_euclid(steps as i64) as usize,
        }
    }

    pub fn steps(&self) -> usize {
        self.hits.len()
    }

    pub fn pulses(&self) -> usize {
        self.hits.iter().filter(|&&h| h).count()
    }

    /// Whether step `i` is an onset (the pattern repeats: any `i`).
    #[inline]
    pub fn hit(&self, i: u64) -> bool {
        let n = self.hits.len() as u64;
        self.hits[((i % n + self.rotation as u64) % n) as usize]
    }

    /// One cycle as played (rotation applied).
    pub fn pattern(&self) -> Vec<bool> {
        (0..self.hits.len() as u64).map(|i| self.hit(i)).collect()
    }

    /// The number of onsets among steps `0 .. n` (for counting bounds before
    /// generating: O(steps)).
    pub fn onsets_before(&self, n: u64) -> u64 {
        let len = self.hits.len() as u64;
        let per_cycle = self.pulses() as u64;
        let rest = (0..n % len).filter(|&i| self.hit(i)).count() as u64;
        n / len * per_cycle + rest
    }
}

/// Bjorklund's algorithm as Toussaint describes it: `pulses` ones and
/// `steps − pulses` zeros paired like Euclid's remainders until at most one
/// group is left over; the canonical
/// (unrotated) pattern, starting with an onset when `pulses > 0`.
pub fn bjorklund(steps: usize, pulses: usize) -> Vec<bool> {
    let pulses = pulses.min(steps);
    if pulses == 0 || pulses == steps {
        return vec![pulses == steps && steps > 0; steps];
    }
    let mut a: Vec<Vec<bool>> = vec![vec![true]; pulses];
    let mut b: Vec<Vec<bool>> = vec![vec![false]; steps - pulses];
    // At least one pairing (E(2,3) = x.x, not xx.), then until at most one
    // remainder group is left.
    loop {
        let n = a.len().min(b.len());
        let mut paired = Vec::with_capacity(n);
        for (x, y) in a.iter().zip(&b) {
            let mut x = x.clone();
            x.extend_from_slice(y);
            paired.push(x);
        }
        b = if a.len() > n {
            a.split_off(n)
        } else {
            b.split_off(n)
        };
        a = paired;
        if b.len() <= 1 {
            break;
        }
    }
    a.into_iter().chain(b).flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(p: &[bool]) -> String {
        p.iter().map(|&h| if h { 'x' } else { '.' }).collect()
    }

    /// The table of Toussaint (2005): every Euclidean rhythm named there.
    #[test]
    fn bjorklund_reproduces_the_published_euclidean_rhythms() {
        for (k, n, want) in [
            (1, 2, "x."),
            (1, 3, "x.."),
            (1, 4, "x..."),
            (4, 12, "x..x..x..x.."),
            (2, 3, "x.x"),
            (2, 5, "x.x.."),
            (3, 4, "x.xx"),
            (3, 5, "x.x.x"),
            (3, 7, "x.x.x.."),
            (3, 8, "x..x..x."),
            (4, 7, "x.x.x.x"),
            (4, 9, "x.x.x.x.."),
            (4, 11, "x..x..x..x."),
            (5, 6, "x.xxxx"),
            (5, 7, "x.xx.xx"),
            (5, 8, "x.xx.xx."),
            (5, 9, "x.x.x.x.x"),
            (5, 11, "x.x.x.x.x.."),
            (5, 12, "x..x.x..x.x."),
            (5, 16, "x..x..x..x..x..."),
            (7, 8, "x.xxxxxx"),
            (7, 12, "x.xx.x.xx.x."),
            (7, 16, "x..x.x.x..x.x.x."),
            (9, 16, "x.xx.x.x.xx.x.x."),
            (11, 24, "x..x.x.x.x.x..x.x.x.x.x."),
            (13, 24, "x.xx.x.x.x.x.xx.x.x.x.x."),
        ] {
            assert_eq!(text(&bjorklund(n, k)), want, "E({k},{n})");
        }
    }

    /// Maximal evenness for every n ≤ 40 and k ≤ n: any window of L
    /// consecutive steps (cyclically) holds ⌊Lk/n⌋ or ⌈Lk/n⌉ onsets, and
    /// the rotation only shifts the pattern.
    #[test]
    fn every_pattern_is_maximally_even_and_rotations_shift_it() {
        for n in 1..=40usize {
            for k in 0..=n {
                let p = bjorklund(n, k);
                assert_eq!(p.len(), n);
                assert_eq!(p.iter().filter(|&&h| h).count(), k, "E({k},{n})");
                assert!(k == 0 || p[0], "E({k},{n}) starts with an onset");
                for len in 1..=n {
                    let (lo, hi) = ((len * k) / n, (len * k).div_ceil(n));
                    for s in 0..n {
                        let c = (0..len).filter(|&i| p[(s + i) % n]).count();
                        assert!(lo <= c && c <= hi, "E({k},{n}) window {s}+{len}: {c}");
                    }
                }
                for r in [-3i64, 0, 1, 5] {
                    let e = Euclid::new(n as u32, k as u32, r);
                    for i in 0..(2 * n as u64) {
                        let j = (i as i64 + r).rem_euclid(n as i64) as usize;
                        assert_eq!(e.hit(i), p[j]);
                    }
                    let brute = (0..3 * n as u64 + 2).filter(|&i| e.hit(i)).count() as u64;
                    assert_eq!(e.onsets_before(3 * n as u64 + 2), brute);
                }
            }
        }
        // Out-of-range settings are clamped, never panic.
        assert_eq!(Euclid::new(0, 5, 0).steps(), 1);
        assert_eq!(Euclid::new(4, 9, 0).pattern(), vec![true; 4]);
        assert_eq!(Euclid::new(9999, 1, 0).steps(), MAX_STEPS as usize);
        assert_eq!(text(&Euclid::new(8, 3, 1).pattern()), "..x..x.x");
    }
}
