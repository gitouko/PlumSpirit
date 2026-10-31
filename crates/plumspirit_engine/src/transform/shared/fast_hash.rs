//! `fast_hash`: hash maps and sets for small integer keys (grid cells,
//! keys, indices) with a fast, deterministic multiply–rotate hasher instead
//! of the standard library's SipHash — the stroke tools look cells up tens of
//! millions of times per run (V09-S39 step 4: the Note Brush's discs,
//! tiles, fills and smudge buckets). Not for keys an attacker chooses: the
//! hasher has no per-process key. Iteration order is deterministic (no
//! random state) but arbitrary; results never depend on it.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// A multiply–rotate hasher over 64-bit words (the Fx scheme).
#[derive(Clone, Copy, Debug, Default)]
pub struct FastHasher(u64);

const K: u64 = 0x517C_C1B7_2722_0A95;

impl Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut w = [0u8; 8];
            w[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(w));
        }
    }

    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(K);
    }

    fn write_u32(&mut self, i: u32) {
        self.write_u64(u64::from(i));
    }

    fn write_u16(&mut self, i: u16) {
        self.write_u64(u64::from(i));
    }

    fn write_u8(&mut self, i: u8) {
        self.write_u64(u64::from(i));
    }

    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }
}

/// A [`HashMap`] with the [`FastHasher`].
pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

/// A [`HashSet`] with the [`FastHasher`].
pub type FastSet<K> = HashSet<K, BuildHasherDefault<FastHasher>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_maps_hold_grid_cells_like_the_standard_map() {
        let mut fast: FastMap<(u64, i64), u32> = FastMap::default();
        let mut std_map: HashMap<(u64, i64), u32> = HashMap::new();
        for c in 0..300u64 {
            for r in -40..40i64 {
                let v = (c * 7 + r.unsigned_abs()) as u32;
                fast.insert((c, r), v);
                std_map.insert((c, r), v);
            }
        }
        assert_eq!(fast.len(), std_map.len());
        for (k, v) in &std_map {
            assert_eq!(fast.get(k), Some(v));
        }
        let mut s: FastSet<(u64, i64)> = FastSet::default();
        assert!(s.insert((1, -1)) && !s.insert((1, -1)) && s.insert((u64::MAX, i64::MIN)));
    }

    #[test]
    fn the_fast_hasher_is_deterministic_and_spreads_neighbouring_cells() {
        let h = |c: u64, r: i64| {
            use std::hash::Hash;
            let mut x = FastHasher::default();
            (c, r).hash(&mut x);
            x.finish()
        };
        assert_eq!(h(5, 6), h(5, 6));
        // Neighbouring cells of a 64 × 64 block fall in distinct buckets of
        // a 4,096-bucket table often enough (no clustering on a few).
        let mut used = std::collections::HashSet::new();
        for c in 0..64u64 {
            for r in 0..64i64 {
                used.insert(h(c, r) >> 52);
            }
        }
        assert!(used.len() > 2500, "{}", used.len());
    }
}
