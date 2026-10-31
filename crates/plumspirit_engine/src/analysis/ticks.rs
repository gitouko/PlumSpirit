//! [`SortedTicks`]: a compact, immutable sorted multiset of ticks that answers
//! "how many values are `<= k`, and what is their sum?" in `O(log n + B)`.
//!
//! Black MIDI is made of stacked chords: many notes start (and end) on the
//! same tick. Values are therefore stored as *entries* — a distinct tick and
//! how many times it occurs — split into blocks of [`BLOCK`] entries. Per block
//! we keep its first tick, the count and tick sum of every value before it,
//! and the bit offset and bit widths of its stream; the stream holds, per
//! entry, the delta from the previous tick and the repeat count − 1, each in
//! the block's fixed width (the bits of its largest delta / count). A block of
//! single, evenly spaced notes costs about one byte per value; a timeline of
//! 12-note chords costs well under a quarter byte per value.
//!
//! A query binary-searches the block firsts and decodes at most `BLOCK - 1`
//! entries of one block (unaligned loads, no branches per bit). Values are
//! never modified after construction: an edit rebuilds the owning summary.

/// Entries (distinct ticks) per block: the most entries one query decodes.
pub const BLOCK: usize = 128;
/// Zero bytes after the bit stream so every read can load 16 bytes.
const PAD: usize = 16;

/// Sorted multiset of `u64` with prefix count / sum queries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SortedTicks {
    len: usize,
    entries: usize,
    /// First tick of each block.
    firsts: Vec<u64>,
    /// Values before each block (count) and their sum.
    counts: Vec<u64>,
    sums: Vec<u128>,
    /// Bit offset of each block's stream in `bits`.
    offsets: Vec<u64>,
    /// Bits per delta / per (count − 1) of each block.
    widths: Vec<(u8, u8)>,
    bits: Vec<u8>,
    total: u128,
    max: u64,
}

/// Bit-stream writer.
struct Writer {
    out: Vec<u8>,
    acc: u128,
    n: u32,
    pos: u64,
}

impl Writer {
    #[inline]
    fn put(&mut self, v: u64, width: u32) {
        if width == 0 {
            return;
        }
        self.acc |= u128::from(v) << self.n;
        self.n += width;
        self.pos += u64::from(width);
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
}

#[inline(always)]
fn read(bits: &[u8], pos: u64, width: u32) -> u64 {
    if width == 0 {
        return 0;
    }
    let byte = (pos >> 3) as usize;
    let shift = (pos & 7) as u32;
    let mask = if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    if width <= 56 {
        let w = u64::from_le_bytes(bits[byte..byte + 8].try_into().expect("8 bytes"));
        (w >> shift) & mask
    } else {
        let w = u128::from_le_bytes(bits[byte..byte + 16].try_into().expect("16 bytes"));
        ((w >> shift) as u64) & mask
    }
}

/// Sequential reader of one block's entries.
struct Entries<'a> {
    bits: &'a [u8],
    pos: u64,
    wd: u32,
    wc: u32,
    value: u64,
    left: usize,
    /// The next entry is the block's first (no delta stored).
    first: bool,
}

impl Entries<'_> {
    /// The next entry's tick (call [`Entries::count`] after it).
    #[inline(always)]
    fn tick(&mut self) -> Option<u64> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        if self.first {
            self.first = false;
        } else {
            self.value += read(self.bits, self.pos, self.wd);
            self.pos += u64::from(self.wd);
        }
        Some(self.value)
    }

    #[inline(always)]
    fn count(&mut self) -> u64 {
        let c = read(self.bits, self.pos, self.wc) + 1;
        self.pos += u64::from(self.wc);
        c
    }
}

fn bits_of(v: u64) -> u32 {
    64 - v.leading_zeros()
}

impl SortedTicks {
    /// Builds from values sorted ascending (debug-asserted).
    pub fn from_sorted(values: &[u64]) -> Self {
        debug_assert!(values.windows(2).all(|w| w[0] <= w[1]));
        // Distinct ticks with their counts.
        let mut runs: Vec<(u64, u64)> = Vec::new();
        for &v in values {
            match runs.last_mut() {
                Some((t, c)) if *t == v => *c += 1,
                _ => runs.push((v, 1)),
            }
        }
        let blocks = runs.len().div_ceil(BLOCK);
        let mut out = SortedTicks {
            len: values.len(),
            entries: runs.len(),
            firsts: Vec::with_capacity(blocks),
            counts: Vec::with_capacity(blocks),
            sums: Vec::with_capacity(blocks),
            offsets: Vec::with_capacity(blocks),
            widths: Vec::with_capacity(blocks),
            bits: Vec::new(),
            total: 0,
            max: values.last().copied().unwrap_or(0),
        };
        let mut w = Writer {
            out: Vec::new(),
            acc: 0,
            n: 0,
            pos: 0,
        };
        let (mut count, mut sum) = (0u64, 0u128);
        for block in runs.chunks(BLOCK) {
            let wd = block
                .windows(2)
                .map(|p| bits_of(p[1].0 - p[0].0))
                .max()
                .unwrap_or(0);
            let wc = block.iter().map(|e| bits_of(e.1 - 1)).max().unwrap_or(0);
            out.firsts.push(block[0].0);
            out.counts.push(count);
            out.sums.push(sum);
            out.offsets.push(w.pos);
            out.widths.push((wd as u8, wc as u8));
            let mut prev = block[0].0;
            for (i, &(t, c)) in block.iter().enumerate() {
                if i > 0 {
                    w.put(t - prev, wd);
                }
                w.put(c - 1, wc);
                prev = t;
                count += c;
                sum += u128::from(t) * u128::from(c);
            }
        }
        if w.n > 0 {
            w.out.push(w.acc as u8);
        }
        w.out.resize(w.out.len() + PAD, 0);
        w.out.shrink_to_fit();
        out.bits = w.out;
        out.total = sum;
        out
    }

    /// The entries of block `b`; the first tick is returned by the first
    /// [`Entries::tick`] call.
    #[inline]
    fn block(&self, b: usize) -> Entries<'_> {
        let (wd, wc) = self.widths[b];
        let n = (self.entries - b * BLOCK).min(BLOCK);
        Entries {
            bits: &self.bits,
            pos: self.offsets[b],
            wd: u32::from(wd),
            wc: u32::from(wc),
            value: self.firsts[b],
            left: n,
            first: true,
        }
    }

    /// Builds from unsorted values (sorts a copy).
    pub fn from_unsorted(mut values: Vec<u64>) -> Self {
        values.sort_unstable();
        SortedTicks::from_sorted(&values)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Smallest value (0 when empty).
    pub fn min(&self) -> u64 {
        self.firsts.first().copied().unwrap_or(0)
    }

    /// Largest value (0 when empty).
    pub fn max(&self) -> u64 {
        self.max
    }

    /// Sum of all values.
    pub fn total(&self) -> u128 {
        self.total
    }

    /// Heap bytes used.
    pub fn heap_bytes(&self) -> usize {
        self.firsts.capacity() * 8
            + self.counts.capacity() * 8
            + self.sums.capacity() * 16
            + self.offsets.capacity() * 8
            + self.widths.capacity() * 2
            + self.bits.capacity()
    }

    /// Number of values `<= k` and their sum.
    pub fn prefix_le(&self, k: u64) -> (usize, u128) {
        if self.len == 0 || k < self.firsts[0] {
            return (0, 0);
        }
        if k >= self.max {
            return (self.len, self.total);
        }
        // Last block whose first tick is <= k; every earlier block is entirely <= k.
        let b = self.firsts.partition_point(|&f| f <= k) - 1;
        let (mut count, mut sum) = (self.counts[b], self.sums[b]);
        let mut e = self.block(b);
        while let Some(t) = e.tick() {
            if t > k {
                break;
            }
            let c = e.count();
            count += c;
            sum += u128::from(t) * u128::from(c);
        }
        (count as usize, sum)
    }

    /// Number of values `<= k`.
    #[inline]
    pub fn count_le(&self, k: u64) -> usize {
        self.prefix_le(k).0
    }

    /// Number and sum of values in `(lo, hi]` (`lo = None`: no lower bound).
    pub fn range(&self, lo: Option<u64>, hi: u64) -> (usize, u128) {
        let (ch, sh) = self.prefix_le(hi);
        match lo {
            Some(l) if l < hi => {
                let (cl, sl) = self.prefix_le(l);
                (ch - cl, sh - sl)
            }
            Some(_) => (0, 0),
            None => (ch, sh),
        }
    }

    /// Calls `f` with every value in `(lo, hi]`, ascending (`lo = None`: from the
    /// first value); a tick that occurs `c` times is passed `c` times.
    pub fn for_each_in(&self, lo: Option<u64>, hi: u64, mut f: impl FnMut(u64)) {
        self.for_each_entry(lo, hi, |t, c| {
            for _ in 0..c {
                f(t);
            }
        });
    }

    /// Calls `f(tick, count)` for every distinct tick in `(lo, hi]`, ascending.
    pub fn for_each_entry(&self, lo: Option<u64>, hi: u64, mut f: impl FnMut(u64, u64)) {
        if self.len == 0 || hi < self.firsts[0] || lo.is_some_and(|l| l >= hi || l >= self.max) {
            return;
        }
        // Start at the last block whose first tick is <= lo (or the first block).
        let mut b = match lo {
            Some(l) => self.firsts.partition_point(|&f| f <= l).saturating_sub(1),
            None => 0,
        };
        let blocks = self.firsts.len();
        while b < blocks {
            let mut e = self.block(b);
            while let Some(t) = e.tick() {
                if t > hi {
                    return;
                }
                let c = e.count();
                if lo.is_none_or(|l| t > l) {
                    f(t, c);
                }
            }
            b += 1;
        }
    }

    /// All values (tests, diagnostics).
    pub fn to_vec(&self) -> Vec<u64> {
        let mut v = Vec::with_capacity(self.len);
        self.for_each_in(None, u64::MAX, |x| v.push(x));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute(values: &[u64], k: u64) -> (usize, u128) {
        let c = values.iter().filter(|&&v| v <= k).count();
        let s = values
            .iter()
            .filter(|&&v| v <= k)
            .map(|&v| u128::from(v))
            .sum();
        (c, s)
    }

    #[test]
    fn prefix_queries_match_a_brute_force_reference() {
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for &n in &[0usize, 1, 2, 127, 128, 129, 1000, 5000] {
            let mut v: Vec<u64> = (0..n)
                .map(|i| match i % 7 {
                    0 => rnd() % 10,
                    1 => u64::MAX - rnd() % 1000,
                    _ => rnd() % 100_000,
                })
                .collect();
            v.sort_unstable();
            let t = SortedTicks::from_sorted(&v);
            assert_eq!(t.len(), n);
            assert_eq!(t.to_vec(), v);
            let mut probes: Vec<u64> = vec![0, 1, 9, 10, 99_999, 100_000, u64::MAX, u64::MAX - 1];
            for _ in 0..200 {
                probes.push(rnd() % 120_000);
            }
            probes.extend(v.iter().copied().take(50));
            for &k in &probes {
                assert_eq!(t.prefix_le(k), brute(&v, k), "n={n} k={k}");
            }
            for _ in 0..100 {
                let a = rnd() % 110_000;
                let b = a + rnd() % 20_000;
                let want: Vec<u64> = v.iter().copied().filter(|&x| x > a && x <= b).collect();
                let mut got = Vec::new();
                t.for_each_in(Some(a), b, |x| got.push(x));
                assert_eq!(got, want);
                let (c, s) = t.range(Some(a), b);
                assert_eq!(c, want.len());
                assert_eq!(s, want.iter().map(|&x| u128::from(x)).sum::<u128>());
            }
        }
    }

    #[test]
    fn duplicates_and_huge_values_are_exact() {
        let v = vec![5u64; 1000];
        let t = SortedTicks::from_sorted(&v);
        assert_eq!(t.prefix_le(4), (0, 0));
        assert_eq!(t.prefix_le(5), (1000, 5000));
        assert!(t.heap_bytes() < 100, "{} bytes", t.heap_bytes());
        // Chords: 12 notes per tick cost far less than a byte each.
        let v: Vec<u64> = (0..12_000u64).map(|i| (i / 12) * 240).collect();
        let t = SortedTicks::from_sorted(&v);
        assert_eq!(t.to_vec(), v);
        assert!(t.heap_bytes() < 12_000 / 4, "{} bytes", t.heap_bytes());
        assert_eq!(
            t.prefix_le(240 * 500),
            (12 * 501, (0..=500u128).map(|i| i * 240 * 12).sum())
        );
        let v = vec![0, u64::MAX / 2, u64::MAX];
        let t = SortedTicks::from_sorted(&v);
        assert_eq!(t.to_vec(), v);
        assert_eq!(t.prefix_le(u64::MAX - 1).0, 2);
        // Every width from 0 to 64 bits round-trips.
        for w in 0..=64u32 {
            let step = if w == 0 { 0 } else { u64::MAX >> (64 - w) };
            let v: Vec<u64> = (0..300u64)
                .map(|i| if i % 2 == 0 { 0 } else { step })
                .scan(0u64, |a, d| {
                    *a = a.saturating_add(d);
                    Some(*a)
                })
                .collect();
            let t = SortedTicks::from_sorted(&v);
            assert_eq!(t.to_vec(), v, "width {w}");
            assert_eq!(
                t.prefix_le(v[150]).0,
                v.iter().filter(|&&x| x <= v[150]).count()
            );
        }
        assert_eq!(t.total(), u128::from(u64::MAX / 2) + u128::from(u64::MAX));
    }
}
