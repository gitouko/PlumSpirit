//! Deterministic randomness for tools (contract §26).
//!
//! Every random decision is a pure function of the tool's seed, a per-decision
//! salt and the identity of the note it is for (its content and track) — never of
//! the evaluation order. A lazy Preview that evaluates only visible notes, the
//! worker that evaluates the whole selection and Apply therefore produce the same
//! values, and Redo reproduces them exactly.

use super::TNote;

/// SplitMix64 finalizer: a fast, well-mixing 64-bit hash step.
#[inline]
pub fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Combines several words into one hash.
#[inline]
pub fn hash(words: &[u64]) -> u64 {
    let mut h = 0x5EED_0F91_u64;
    for &w in words {
        h = mix(h ^ w);
    }
    h
}

/// Salts that keep independent decisions of one tool uncorrelated.
pub mod salt {
    pub const TICK: u64 = 1;
    pub const GATE: u64 = 2;
    pub const VELOCITY: u64 = 3;
    pub const PITCH: u64 = 4;
    pub const KEEP: u64 = 5;
    pub const ORDER: u64 = 6;
    pub const SUB: u64 = 7;
    pub const SHAPE: u64 = 8;
    /// Paste Repeat (clips): which generations mutate, and how.
    pub const MUTATE: u64 = 9;
    /// Note Properties' math expressions: each note's `rnd` (B1-0076).
    pub const EXPR: u64 = 10;
}

/// A small deterministic generator (SplitMix64 stream).
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    #[inline]
    pub fn new(seed: u64) -> Rng {
        Rng {
            state: mix(seed ^ 0xA076_1D64_78BD_642F),
        }
    }

    /// The generator for one note's decisions of kind `salt`.
    #[inline]
    pub fn for_note(seed: u64, salt: u64, n: &TNote) -> Rng {
        Rng::new(hash(&[seed, salt, note_key(n)]))
    }

    /// The generator for decision `index` (positions, generated items).
    #[inline]
    pub fn for_index(seed: u64, salt: u64, index: u64) -> Rng {
        Rng::new(hash(&[seed, salt, index]))
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    #[inline]
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform in [lo, hi] (inclusive integers; `lo > hi` is swapped).
    #[inline]
    pub fn range_i64(&mut self, lo: i64, hi: i64) -> i64 {
        let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        let span = (hi as i128 - lo as i128 + 1) as u128;
        if span > u64::MAX as u128 {
            return self.next_u64() as i64;
        }
        let r = (self.next_u64() as u128 * span) >> 64;
        (lo as i128 + r as i128) as i64
    }

    /// Uniform in [lo, hi).
    #[inline]
    pub fn range_f64(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// Standard normal (Box–Muller).
    #[inline]
    pub fn normal(&mut self) -> f64 {
        let u1 = self.unit().max(1e-300);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Triangular on [-1, 1] with mode 0.
    #[inline]
    pub fn triangular(&mut self) -> f64 {
        self.unit() + self.unit() - 1.0
    }

    /// True with probability `p`.
    #[inline]
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// A value in [-1, 1] from distribution `d`.
    #[inline]
    pub fn bipolar(&mut self, d: Distribution) -> f64 {
        match d {
            Distribution::Uniform => self.unit() * 2.0 - 1.0,
            // ±1 ≈ 3 standard deviations, clipped.
            Distribution::Gaussian => (self.normal() / 3.0).clamp(-1.0, 1.0),
            Distribution::Triangular => self.triangular(),
            Distribution::Exponential => {
                let e = -(1.0 - self.unit()).max(1e-300).ln() / 3.0;
                let s = if self.unit() < 0.5 { -1.0 } else { 1.0 };
                (s * e).clamp(-1.0, 1.0)
            }
        }
    }

    /// v0.9 (V09-S34 `seeded_stream`): uniform in `0 .. n` without modulo
    /// bias (0 when `n` is 0).
    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        // Lemire's multiply-shift with rejection of the biased zone.
        let zone = n.wrapping_neg() % n;
        loop {
            let m = u128::from(self.next_u64()) * u128::from(n);
            if (m as u64) >= zone {
                return (m >> 64) as u64;
            }
        }
    }

    /// A uniformly random permutation of `v` (Fisher–Yates).
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            v.swap(i, j);
        }
    }

    /// An index drawn with probability proportional to `weights` (negative
    /// and non-finite weights count as 0); `None` when every weight is 0.
    pub fn weighted(&mut self, weights: &[f64]) -> Option<usize> {
        let w = |x: f64| if x.is_finite() && x > 0.0 { x } else { 0.0 };
        let total: f64 = weights.iter().map(|&x| w(x)).sum();
        if total <= 0.0 {
            return None;
        }
        let mut r = self.unit() * total;
        let mut last = None;
        for (i, &x) in weights.iter().enumerate() {
            let x = w(x);
            if x > 0.0 {
                last = Some(i);
                if r < x {
                    return Some(i);
                }
                r -= x;
            }
        }
        last
    }
}

/// Distribution shapes offered by random tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distribution {
    Uniform,
    Gaussian,
    Triangular,
    Exponential,
}

impl Distribution {
    pub const NAMES: &'static [&'static str] =
        &["Uniform", "Gaussian", "Triangular", "Exponential"];

    pub fn from_index(i: usize) -> Distribution {
        match i {
            1 => Distribution::Gaussian,
            2 => Distribution::Triangular,
            3 => Distribution::Exponential,
            _ => Distribution::Uniform,
        }
    }
}

/// Identity of a note for seeding: content + track slot. Identical stacked
/// duplicates get identical decisions (they stay stacked).
#[inline]
pub fn note_key(n: &TNote) -> u64 {
    hash(&[
        n.start,
        n.dur,
        u64::from(n.pitch)
            | (u64::from(n.vel) << 8)
            | (u64::from(n.ch) << 16)
            | (u64::from(n.rel) << 24),
        u64::from(n.sub) | (u64::from(n.track) << 32),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_are_reproducible_and_seed_dependent() {
        let a: Vec<u64> = (0..5)
            .map(|_| 0)
            .scan(Rng::new(7), |r, _| Some(r.next_u64()))
            .collect();
        let b: Vec<u64> = (0..5)
            .map(|_| 0)
            .scan(Rng::new(7), |r, _| Some(r.next_u64()))
            .collect();
        let c: Vec<u64> = (0..5)
            .map(|_| 0)
            .scan(Rng::new(8), |r, _| Some(r.next_u64()))
            .collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn ranges_stay_inside_their_bounds() {
        let mut r = Rng::new(3);
        for _ in 0..10_000 {
            let v = r.range_i64(-5, 5);
            assert!((-5..=5).contains(&v));
            let u = r.unit();
            assert!((0.0..1.0).contains(&u));
            for d in [
                Distribution::Uniform,
                Distribution::Gaussian,
                Distribution::Triangular,
                Distribution::Exponential,
            ] {
                let b = r.bipolar(d);
                assert!((-1.0..=1.0).contains(&b));
            }
        }
        assert_eq!(r.range_i64(4, 4), 4);
        let _ = r.range_i64(i64::MIN, i64::MAX);
    }
}
