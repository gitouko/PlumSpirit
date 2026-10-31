//! The per-chunk subchannel column of the note store, stored as narrow as its
//! largest value allows (v0.5).
//!
//! Most tracks never need it (every note in the main subchannel: there is no
//! column and it costs nothing). A track imported with several MIDI channels gets
//! one subchannel per channel (ids 0–15): one byte per note instead of four. Up
//! to 65,535 subchannels per track cost two bytes per note; only ids above that
//! use four. The column widens automatically when a larger id is stored and never
//! narrows by itself (it is rebuilt narrow when a chunk is rebuilt).
//!
//! Invariant kept by the note store: a column is either absent ([`Col::Main`] or
//! an empty vector) or holds exactly one entry per note of its chunk.

#[derive(Clone, Debug, Default)]
enum Col {
    /// No column: every note is in subchannel 0.
    #[default]
    Main,
    W8(Vec<u8>),
    W16(Vec<u16>),
    W32(Vec<u32>),
}

/// Subchannel ids of a chunk's notes; empty = every note is in subchannel 0.
#[derive(Clone, Debug, Default)]
pub(crate) struct SubColumn(Col);

/// A borrowed view of a column for tight loops (the width is matched once
/// per chunk by the caller, then once per note on a predictable branch).
#[derive(Clone, Copy, Debug)]
pub(crate) enum SubRef<'a> {
    Main,
    W8(&'a [u8]),
    W16(&'a [u16]),
    W32(&'a [u32]),
}

impl SubRef<'_> {
    #[inline(always)]
    pub(crate) fn get(self, i: usize) -> u32 {
        match self {
            SubRef::Main => 0,
            SubRef::W8(v) => u32::from(v[i]),
            SubRef::W16(v) => u32::from(v[i]),
            SubRef::W32(v) => v[i],
        }
    }
}

/// Bytes per entry needed for `v`.
#[inline]
fn width_of(v: u32) -> u8 {
    if v <= u32::from(u8::MAX) {
        1
    } else if v <= u32::from(u16::MAX) {
        2
    } else {
        4
    }
}

impl SubColumn {
    #[inline]
    pub(crate) fn as_ref(&self) -> SubRef<'_> {
        match &self.0 {
            Col::Main => SubRef::Main,
            Col::W8(v) if v.is_empty() => SubRef::Main,
            Col::W8(v) => SubRef::W8(v),
            Col::W16(v) => SubRef::W16(v),
            Col::W32(v) => SubRef::W32(v),
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        match &self.0 {
            Col::Main => 0,
            Col::W8(v) => v.len(),
            Col::W16(v) => v.len(),
            Col::W32(v) => v.len(),
        }
    }

    #[inline]
    fn width(&self) -> u8 {
        match &self.0 {
            Col::Main | Col::W8(_) => 1,
            Col::W16(_) => 2,
            Col::W32(_) => 4,
        }
    }

    /// Value of note `i`: 0 without a column (the column, when present, holds
    /// every note of the chunk).
    #[inline]
    pub(crate) fn get(&self, i: usize) -> u32 {
        match &self.0 {
            Col::Main => 0,
            Col::W8(v) => u32::from(v[i]),
            Col::W16(v) => u32::from(v[i]),
            Col::W32(v) => v[i],
        }
    }

    /// Converts to a width that can hold `v` (keeps length and capacity).
    #[cold]
    #[inline(never)]
    fn widen_for(&mut self, v: u32) {
        let need = width_of(v);
        if need <= self.width() && !matches!(self.0, Col::Main) {
            return;
        }
        let cap = self.capacity();
        let vals: Vec<u32> = self.iter().collect();
        self.0 = match need {
            1 => {
                let mut w = Vec::with_capacity(cap);
                w.extend(vals.iter().map(|&x| x as u8));
                Col::W8(w)
            }
            2 => {
                let mut w = Vec::with_capacity(cap);
                w.extend(vals.iter().map(|&x| x as u16));
                Col::W16(w)
            }
            _ => {
                let mut w = Vec::with_capacity(cap);
                w.extend_from_slice(&vals);
                Col::W32(w)
            }
        };
    }

    #[inline]
    fn capacity(&self) -> usize {
        match &self.0 {
            Col::Main => 0,
            Col::W8(v) => v.capacity(),
            Col::W16(v) => v.capacity(),
            Col::W32(v) => v.capacity(),
        }
    }

    /// Heap bytes held.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.capacity() * usize::from(self.width())
    }

    pub(crate) fn reserve_exact(&mut self, extra: usize) {
        match &mut self.0 {
            Col::Main => self.0 = Col::W8(Vec::with_capacity(extra)),
            Col::W8(v) => v.reserve_exact(extra),
            Col::W16(v) => v.reserve_exact(extra),
            Col::W32(v) => v.reserve_exact(extra),
        }
    }

    /// Grows (with zeros) or shrinks to `n` entries.
    pub(crate) fn resize_zero(&mut self, n: usize) {
        match &mut self.0 {
            Col::Main => {
                if n > 0 {
                    self.0 = Col::W8(vec![0; n]);
                }
            }
            Col::W8(v) => v.resize(n, 0),
            Col::W16(v) => v.resize(n, 0),
            Col::W32(v) => v.resize(n, 0),
        }
    }

    #[inline]
    pub(crate) fn push(&mut self, x: u32) {
        match &mut self.0 {
            Col::W8(v) if x <= u32::from(u8::MAX) => v.push(x as u8),
            Col::W16(v) if x <= u32::from(u16::MAX) => v.push(x as u16),
            Col::W32(v) => v.push(x),
            _ => {
                self.widen_for(x);
                self.push(x);
            }
        }
    }

    pub(crate) fn insert(&mut self, i: usize, x: u32) {
        match &mut self.0 {
            Col::W8(v) if x <= u32::from(u8::MAX) => v.insert(i, x as u8),
            Col::W16(v) if x <= u32::from(u16::MAX) => v.insert(i, x as u16),
            Col::W32(v) => v.insert(i, x),
            _ => {
                self.widen_for(x);
                self.insert(i, x);
            }
        }
    }

    /// Sets note `i` (the column must hold it).
    #[inline]
    pub(crate) fn set(&mut self, i: usize, x: u32) {
        match &mut self.0 {
            Col::W8(v) if x <= u32::from(u8::MAX) => v[i] = x as u8,
            Col::W16(v) if x <= u32::from(u16::MAX) => v[i] = x as u16,
            Col::W32(v) => v[i] = x,
            Col::Main => panic!("set({i}) on an absent subchannel column"),
            _ => {
                self.widen_for(x);
                self.set(i, x);
            }
        }
    }

    /// Keeps the entries whose index passes `keep` (in order); one tight loop
    /// per width.
    pub(crate) fn retain_indices(&mut self, keep: impl Fn(usize) -> bool) {
        fn go<T: Copy>(v: &mut Vec<T>, keep: impl Fn(usize) -> bool) {
            let mut w = 0;
            for r in 0..v.len() {
                if keep(r) {
                    v[w] = v[r];
                    w += 1;
                }
            }
            v.truncate(w);
        }
        match &mut self.0 {
            Col::Main => {}
            Col::W8(v) => go(v, keep),
            Col::W16(v) => go(v, keep),
            Col::W32(v) => go(v, keep),
        }
    }

    /// Drops the column (every note back in subchannel 0).
    pub(crate) fn clear(&mut self) {
        self.0 = Col::Main;
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        match &mut self.0 {
            Col::Main => {}
            Col::W8(v) => v.shrink_to_fit(),
            Col::W16(v) => v.shrink_to_fit(),
            Col::W32(v) => v.shrink_to_fit(),
        }
    }

    /// Appends every entry of `other`.
    pub(crate) fn extend_from(&mut self, other: &SubColumn) {
        let max = other.iter().max().unwrap_or(0);
        if width_of(max) > self.width() || matches!(self.0, Col::Main) {
            self.widen_for(max);
        }
        match (&mut self.0, &other.0) {
            (_, Col::Main) => {}
            (Col::W8(a), Col::W8(b)) => a.extend_from_slice(b),
            (Col::W16(a), Col::W16(b)) => a.extend_from_slice(b),
            (Col::W32(a), Col::W32(b)) => a.extend_from_slice(b),
            _ => {
                for x in other.iter() {
                    self.push(x);
                }
            }
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        let (a, b, c): (&[u8], &[u16], &[u32]) = match &self.0 {
            Col::Main => (&[], &[], &[]),
            Col::W8(v) => (v, &[], &[]),
            Col::W16(v) => (&[], v, &[]),
            Col::W32(v) => (&[], &[], v),
        };
        a.iter()
            .map(|&x| u32::from(x))
            .chain(b.iter().map(|&x| u32::from(x)))
            .chain(c.iter().copied())
    }

    /// Whether every entry is 0 (the column can be dropped).
    pub(crate) fn all_zero(&self) -> bool {
        match &self.0 {
            Col::Main => true,
            Col::W8(v) => v.iter().all(|&x| x == 0),
            Col::W16(v) => v.iter().all(|&x| x == 0),
            Col::W32(v) => v.iter().all(|&x| x == 0),
        }
    }

    /// Entries equal to `x`.
    pub(crate) fn count_eq(&self, x: u32) -> usize {
        match &self.0 {
            Col::Main => 0,
            Col::W8(v) => u8::try_from(x).map_or(0, |x| v.iter().filter(|&&s| s == x).count()),
            Col::W16(v) => u16::try_from(x).map_or(0, |x| v.iter().filter(|&&s| s == x).count()),
            Col::W32(v) => v.iter().filter(|&&s| s == x).count(),
        }
    }

    /// Builds a column of `len` values, none above `max` (the width is chosen
    /// once from `max`).
    pub(crate) fn from_values(
        values: impl Iterator<Item = u32>,
        len: usize,
        max: u32,
    ) -> SubColumn {
        SubColumn(match width_of(max) {
            1 => {
                let mut v = Vec::with_capacity(len);
                v.extend(values.map(|x| x as u8));
                Col::W8(v)
            }
            2 => {
                let mut v = Vec::with_capacity(len);
                v.extend(values.map(|x| x as u16));
                Col::W16(v)
            }
            _ => {
                let mut v = Vec::with_capacity(len);
                v.extend(values);
                Col::W32(v)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_widens_only_when_needed_and_keeps_values() {
        let mut c = SubColumn::default();
        assert!(c.is_empty());
        assert_eq!(c.heap_bytes(), 0);
        assert_eq!(c.get(5), 0, "no column: subchannel 0");
        for v in 0..16 {
            c.push(v);
        }
        assert_eq!(c.heap_bytes(), c.capacity());
        assert_eq!(c.width(), 1);
        c.set(3, 300);
        assert_eq!(c.width(), 2);
        c.insert(0, 70_000);
        assert_eq!(c.width(), 4);
        let v: Vec<u32> = c.iter().collect();
        assert_eq!(v[0], 70_000);
        assert_eq!(v[4], 300);
        assert_eq!(v[16], 15);
        assert_eq!(c.count_eq(300), 1);
        let mut d = SubColumn::default();
        d.push(1);
        d.extend_from(&c);
        assert_eq!(d.len(), 18);
        assert_eq!(d.get(1), 70_000);
        d.retain_indices(|i| i < 2);
        assert_eq!(d.len(), 2);
        assert!(!d.all_zero());
        d.clear();
        assert!(d.is_empty() && d.width() == 1);
        let e = SubColumn::from_values([0u32, 0, 0].into_iter(), 3, 0);
        assert!(e.all_zero());
        assert_eq!(
            SubColumn::from_values([1u32, 300].into_iter(), 2, 300).width(),
            2
        );
        let mut f = SubColumn::default();
        f.resize_zero(4);
        assert_eq!((f.len(), f.get(3)), (4, 0));
        f.set(2, 9);
        assert_eq!(f.get(2), 9);
        let mut g = SubColumn::default();
        g.extend_from(&f);
        assert_eq!(g.iter().collect::<Vec<_>>(), vec![0, 0, 9, 0]);
    }
}
