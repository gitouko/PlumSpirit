//! Minimal growable bitset used for per-chunk note selection.
//!
//! Selection is stored as one bit per note, parallel to the note arrays of a chunk,
//! so selecting a million notes costs ~122 KiB instead of a million hash entries, and
//! "select all" touches 64 notes per machine word.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BitSet {
    words: Vec<u64>,
    len: usize,
}

#[inline]
fn words_for(len: usize) -> usize {
    len.div_ceil(64)
}

impl BitSet {
    pub fn new() -> Self {
        BitSet::default()
    }

    pub fn with_capacity(bits: usize) -> Self {
        BitSet {
            words: Vec::with_capacity(words_for(bits)),
            len: 0,
        }
    }

    /// `len` bits, all set to `value`.
    pub fn filled(len: usize, value: bool) -> Self {
        let mut s = BitSet {
            words: vec![if value { u64::MAX } else { 0 }; words_for(len)],
            len,
        };
        s.clear_tail();
        s
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Clears bits beyond `len` in the last word so `count_ones` stays exact.
    fn clear_tail(&mut self) {
        let rem = self.len % 64;
        if rem != 0
            && let Some(last) = self.words.last_mut()
        {
            *last &= (1u64 << rem) - 1;
        }
    }

    #[inline]
    pub fn get(&self, i: usize) -> bool {
        debug_assert!(i < self.len);
        (self.words[i / 64] >> (i % 64)) & 1 == 1
    }

    /// Sets bit `i`; returns true if the value changed.
    #[inline]
    pub fn set(&mut self, i: usize, value: bool) -> bool {
        debug_assert!(i < self.len);
        let w = &mut self.words[i / 64];
        let mask = 1u64 << (i % 64);
        let old = *w & mask != 0;
        if value {
            *w |= mask;
        } else {
            *w &= !mask;
        }
        old != value
    }

    #[inline]
    pub fn push(&mut self, value: bool) {
        if self.len.is_multiple_of(64) {
            self.words.push(0);
        }
        let i = self.len;
        self.len += 1;
        if value {
            self.words[i / 64] |= 1u64 << (i % 64);
        }
    }

    /// Appends `n` cleared bits.
    pub fn push_zeros(&mut self, n: usize) {
        self.len += n;
        self.words.resize(words_for(self.len), 0);
    }

    /// Inserts a bit at `index`, shifting later bits up by one position.
    pub fn insert(&mut self, index: usize, value: bool) {
        assert!(index <= self.len, "bitset insert out of range");
        self.push(false);
        let wi = index / 64;
        let bi = index % 64;
        // Shift every later word left by one, carrying the top bit into the next word.
        let last = self.words.len() - 1;
        let mut w = last;
        while w > wi {
            let carry = self.words[w - 1] >> 63;
            self.words[w] = (self.words[w] << 1) | carry;
            w -= 1;
        }
        let word = self.words[wi];
        let low_mask = if bi == 0 { 0 } else { (1u64 << bi) - 1 };
        let low = word & low_mask;
        let high = (word & !low_mask) << 1;
        let bit = if value { 1u64 << bi } else { 0 };
        self.words[wi] = low | bit | high;
        self.clear_tail();
    }

    /// Sets every bit to `value`; returns the number of set bits afterwards.
    pub fn fill(&mut self, value: bool) -> usize {
        let v = if value { u64::MAX } else { 0 };
        for w in &mut self.words {
            *w = v;
        }
        self.clear_tail();
        if value { self.len } else { 0 }
    }

    pub fn count_ones(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Splits off bits `[at, len)` into a new bitset.
    pub fn split_off(&mut self, at: usize) -> BitSet {
        assert!(at <= self.len);
        let mut tail = BitSet::with_capacity(self.len - at);
        for i in at..self.len {
            tail.push(self.get(i));
        }
        self.len = at;
        self.words.truncate(words_for(at));
        self.clear_tail();
        tail
    }

    /// Iterates indices of set bits in ascending order.
    pub fn iter_ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(wi, &word)| {
            let mut w = word;
            std::iter::from_fn(move || {
                if w == 0 {
                    None
                } else {
                    let tz = w.trailing_zeros() as usize;
                    w &= w - 1;
                    Some(wi * 64 + tz)
                }
            })
        })
    }

    /// A hash of the bits and the length (v0.9 V09-S19: selection analytics
    /// reuse a chunk's analysis while its selection bits are unchanged).
    /// O(words).
    pub fn fingerprint(&self) -> u64 {
        let mut h = 0x9E37_79B9_7F4A_7C15u64 ^ self.len as u64;
        for &w in &self.words {
            h = (h ^ w).wrapping_mul(0xFF51_AFD7_ED55_8CCD).rotate_left(29);
        }
        h
    }

    /// Approximate heap bytes owned.
    pub fn heap_bytes(&self) -> usize {
        self.words.capacity() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_vec(b: &BitSet) -> Vec<bool> {
        (0..b.len()).map(|i| b.get(i)).collect()
    }

    #[test]
    fn insert_matches_vec_model() {
        // Deterministic pseudo-random model check across word boundaries.
        let mut model: Vec<bool> = Vec::new();
        let mut bits = BitSet::new();
        let mut seed = 0x1234_5678_9abc_def0u64;
        for _ in 0..600 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let idx = if model.is_empty() {
                0
            } else {
                (seed as usize) % (model.len() + 1)
            };
            let val = (seed >> 40) & 1 == 1;
            model.insert(idx, val);
            bits.insert(idx, val);
            assert_eq!(to_vec(&bits), model);
            assert_eq!(bits.count_ones(), model.iter().filter(|b| **b).count());
        }
    }

    #[test]
    fn split_fill_iter() {
        let mut b = BitSet::filled(130, false);
        b.set(0, true);
        b.set(64, true);
        b.set(129, true);
        assert_eq!(b.iter_ones().collect::<Vec<_>>(), vec![0, 64, 129]);
        let tail = b.split_off(64);
        assert_eq!(b.len(), 64);
        assert_eq!(tail.len(), 66);
        assert_eq!(b.count_ones(), 1);
        assert_eq!(tail.iter_ones().collect::<Vec<_>>(), vec![0, 65]);
        let mut f = BitSet::filled(70, true);
        assert_eq!(f.count_ones(), 70);
        assert_eq!(f.fill(false), 0);
        assert_eq!(f.count_ones(), 0);
        assert_eq!(f.fill(true), 70);
        assert_eq!(f.count_ones(), 70);
    }
}
