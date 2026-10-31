//! `seeded_stream`: deterministic randomness for the new functions, on
//! [`super::super::rng`] — every decision a pure function of the tool's
//! seed, the function's own salt and a key (a note, a chord, a period, a
//! region), never of the evaluation order, so the lazy Preview, the worker
//! and Apply agree and editing one region leaves the others' choices alone.
//!
//! * [`salt_of`] gives each function its own salt from its stable id
//!   (`n09`), distinct from the shared [`super::super::rng::salt`]
//!   constants: a new function never repeats an older one's dice.
//! * [`stream`] is the generator for one key; [`region_seed`] derives a
//!   region's own seed (seed lattices).
//! * [`Rng::below`], [`Rng::shuffle`] and [`Rng::weighted`] are the draws
//!   the designs use (unbiased ranges, permutations, weighted choices).

pub use super::super::rng::{Rng, hash, mix, note_key};

/// A function's salt from its stable id (FNV-1a, top bit set so it never
/// equals a small shared salt).
pub fn salt_of(id: &str) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for b in id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01B3);
    }
    h | 1 << 63
}

/// The generator for the decisions of kind `salt` about `key` (a note's
/// [`note_key`], a chord's first note, a period or region index, …).
/// No allocation: the same value as `hash(&[seed, salt, key…])`.
pub fn stream(seed: u64, salt: u64, key: &[u64]) -> Rng {
    let mut h = hash(&[seed, salt]);
    for &w in key {
        h = mix(h ^ w);
    }
    Rng::new(h)
}

/// The seed of region `index` (a bar, N bars, a marker section): what the
/// region's own decisions are seeded with, so they do not depend on any
/// other region.
pub fn region_seed(seed: u64, salt: u64, index: u64) -> u64 {
    hash(&[seed, salt, 0x5E6_10E, index])
}

#[cfg(test)]
mod tests {
    use super::super::super::rng::salt;
    use super::*;

    #[test]
    fn salts_are_distinct_and_streams_depend_only_on_their_key() {
        let ids: Vec<String> = (1..=99).map(|k| format!("n{k:02}")).collect();
        let mut salts: Vec<u64> = ids.iter().map(|i| salt_of(i)).collect();
        salts.extend([salt::TICK, salt::GATE, salt::MUTATE]);
        let n = salts.len();
        salts.sort_unstable();
        salts.dedup();
        assert_eq!(salts.len(), n);
        let a: Vec<u64> = {
            let mut r = stream(7, salt_of("n09"), &[3, 4]);
            (0..4).map(|_| r.next_u64()).collect()
        };
        let mut r = stream(7, salt_of("n09"), &[3, 4]);
        assert_eq!(a, (0..4).map(|_| r.next_u64()).collect::<Vec<_>>());
        let mut other = stream(7, salt_of("n09"), &[3, 5]);
        assert_ne!(a[0], other.next_u64());
        assert_ne!(region_seed(7, 1, 0), region_seed(7, 1, 1));
        let mut h = Rng::new(hash(&[7, salt_of("n09"), 3, 4]));
        assert_eq!(a[0], h.next_u64());
        assert_eq!(region_seed(7, 1, 2), region_seed(7, 1, 2));
    }

    #[test]
    fn draws_are_unbiased_permutations_and_weighted_choices() {
        let mut r = Rng::new(1);
        // below(n) covers 0 .. n roughly evenly.
        let mut hist = [0u32; 6];
        for _ in 0..60_000 {
            hist[r.below(6) as usize] += 1;
        }
        assert!(
            hist.iter().all(|&h| (9_000..11_000).contains(&h)),
            "{hist:?}"
        );
        assert_eq!(r.below(0), 0);
        assert_eq!(r.below(1), 0);
        // shuffle permutes.
        let mut v: Vec<u32> = (0..50).collect();
        r.shuffle(&mut v);
        let mut s = v.clone();
        s.sort_unstable();
        assert_eq!(s, (0..50).collect::<Vec<_>>());
        assert_ne!(v, s);
        // weighted: proportional, zeros never chosen, all-zero → None.
        let mut hist = [0u32; 4];
        for _ in 0..40_000 {
            hist[r.weighted(&[1.0, 0.0, 3.0, f64::NAN]).unwrap()] += 1;
        }
        assert_eq!((hist[1], hist[3]), (0, 0));
        assert!((9_000..11_000).contains(&hist[0]), "{hist:?}");
        assert_eq!(r.weighted(&[0.0, -1.0]), None);
        assert_eq!(r.weighted(&[]), None);
    }
}
