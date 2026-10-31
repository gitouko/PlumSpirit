//! Minimal unsigned 256-bit integer for exact arithmetic.
//!
//! Used by the exact scale factors (`editor::factor`, slow path for factors with
//! many significant digits) and by the tempo map (`model::tempo`), which converts
//! between ticks and audio frames through exact rationals instead of floating point.

use std::cmp::Ordering;

/// Little-endian 64-bit limbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct U256([u64; 4]);

impl U256 {
    pub const ZERO: U256 = U256([0; 4]);
    pub const ONE: U256 = U256([1, 0, 0, 0]);

    pub fn from_u128(v: u128) -> Self {
        U256([v as u64, (v >> 64) as u64, 0, 0])
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 4]
    }

    /// `Some(v)` if the value fits in a `u64`.
    pub fn to_u64(self) -> Option<u64> {
        (self.0[1] == 0 && self.0[2] == 0 && self.0[3] == 0).then_some(self.0[0])
    }

    /// `Some(v)` if the value fits in a `u128`.
    pub fn to_u128(self) -> Option<u128> {
        (self.0[2] == 0 && self.0[3] == 0)
            .then_some(u128::from(self.0[0]) | (u128::from(self.0[1]) << 64))
    }

    pub fn checked_mul_u64(&self, m: u64) -> Option<U256> {
        let mut out = [0u64; 4];
        let mut carry: u128 = 0;
        for (i, limb) in self.0.iter().enumerate() {
            let p = u128::from(*limb) * u128::from(m) + carry;
            out[i] = p as u64;
            carry = p >> 64;
        }
        (carry == 0).then_some(U256(out))
    }

    /// Divides by a small value, returning (quotient, remainder).
    pub fn div_rem_u64(&self, d: u64) -> (U256, u64) {
        assert!(d != 0, "division by zero");
        let mut out = [0u64; 4];
        let mut rem: u128 = 0;
        for i in (0..4).rev() {
            let cur = (rem << 64) | u128::from(self.0[i]);
            out[i] = (cur / u128::from(d)) as u64;
            rem = cur % u128::from(d);
        }
        (U256(out), rem as u64)
    }

    fn bits(&self) -> u32 {
        for i in (0..4).rev() {
            if self.0[i] != 0 {
                return (i as u32) * 64 + (64 - self.0[i].leading_zeros());
            }
        }
        0
    }

    fn bit(&self, i: u32) -> bool {
        (self.0[(i / 64) as usize] >> (i % 64)) & 1 == 1
    }

    fn set_bit(&mut self, i: u32) {
        self.0[(i / 64) as usize] |= 1 << (i % 64);
    }

    /// Shift left by one; returns `None` if a bit would be lost.
    fn shl1(&self) -> Option<U256> {
        if self.0[3] >> 63 == 1 {
            return None;
        }
        let mut out = [0u64; 4];
        for i in (0..4).rev() {
            out[i] = (self.0[i] << 1) | if i > 0 { self.0[i - 1] >> 63 } else { 0 };
        }
        Some(U256(out))
    }

    /// `self + 1`; the caller guarantees no overflow.
    fn add_one(&self) -> U256 {
        let mut out = self.0;
        for limb in &mut out {
            let (v, carry) = limb.overflowing_add(1);
            *limb = v;
            if !carry {
                break;
            }
        }
        U256(out)
    }

    /// `self - o`; requires `self >= o`.
    pub fn sub(&self, o: &U256) -> U256 {
        debug_assert!(*self >= *o);
        let mut out = [0u64; 4];
        let mut borrow = 0u64;
        for (i, slot) in out.iter_mut().enumerate() {
            let (d1, b1) = self.0[i].overflowing_sub(o.0[i]);
            let (d2, b2) = d1.overflowing_sub(borrow);
            *slot = d2;
            borrow = u64::from(b1 || b2);
        }
        U256(out)
    }

    /// Long division: (quotient, remainder). `d` must be non-zero.
    pub fn div_rem(&self, d: &U256) -> (U256, U256) {
        assert!(!d.is_zero(), "division by zero");
        if *self < *d {
            return (U256::ZERO, *self);
        }
        let mut q = U256::ZERO;
        let mut r = U256::ZERO;
        for i in (0..self.bits()).rev() {
            // r = (r << 1) | bit; r < d <= 2^256-1 so r << 1 fits unless d's top bit
            // is set, in which case compare before shifting.
            match r.shl1() {
                Some(mut s) => {
                    if self.bit(i) {
                        s.0[0] |= 1;
                    }
                    r = s;
                    if r >= *d {
                        r = r.sub(d);
                        q.set_bit(i);
                    }
                }
                None => {
                    // r >= 2^255, so 2r + bit >= 2^256 > d and the quotient bit is 1.
                    // New remainder 2r + bit - d computed as r - (d - r) + bit, which
                    // stays below d because r <= d - 1.
                    let diff = d.sub(&r);
                    let mut nr = r.sub(&diff);
                    if self.bit(i) {
                        nr = nr.add_one();
                    }
                    r = nr;
                    q.set_bit(i);
                }
            }
        }
        (q, r)
    }

    /// Exact product of two `u128` values.
    pub fn mul_u128_u128(a: u128, b: u128) -> U256 {
        let lo = U256::mul_u64_u128(a as u64, b);
        let hi = U256::mul_u64_u128((a >> 64) as u64, b);
        // hi << 64 cannot overflow: a < 2^128 and b < 2^128.
        let shifted = U256([0, hi.0[0], hi.0[1], hi.0[2]]);
        lo.checked_add(&shifted).unwrap_or(U256([u64::MAX; 4]))
    }

    pub fn checked_add(&self, o: &U256) -> Option<U256> {
        let mut out = [0u64; 4];
        let mut carry = 0u128;
        for (i, slot) in out.iter_mut().enumerate() {
            let s = u128::from(self.0[i]) + u128::from(o.0[i]) + carry;
            *slot = s as u64;
            carry = s >> 64;
        }
        (carry == 0).then_some(U256(out))
    }

    /// Exact product of a `u64` and a `u128` (at most 192 bits).
    pub fn mul_u64_u128(a: u64, b: u128) -> U256 {
        let lo = u128::from(a) * u128::from(b as u64);
        let hi = u128::from(a) * (b >> 64);
        // (hi << 64) + lo
        let l0 = lo as u64;
        let mid = (lo >> 64) + (hi & u128::from(u64::MAX));
        let l1 = mid as u64;
        let l2 = (hi >> 64) + (mid >> 64);
        U256([l0, l1, l2 as u64, (l2 >> 64) as u64])
    }
}

impl PartialOrd for U256 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for U256 {
    fn cmp(&self, other: &Self) -> Ordering {
        for i in (0..4).rev() {
            match self.0[i].cmp(&other.0[i]) {
                Ordering::Equal => continue,
                o => return o,
            }
        }
        Ordering::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_and_div_agree_with_u128() {
        let mut seed = 99u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let a = next();
            let b = u128::from(next()) | (u128::from(next() >> 3) << 64);
            let p = U256::mul_u64_u128(a, b);
            let d = u128::from(next() | 1);
            // Check (a*b) / d via reconstruction: q*d + r == a*b.
            let (q, r) = p.div_rem(&U256::from_u128(d));
            let r = r.to_u128().unwrap();
            assert!(r < d);
            // q * d + r == p
            let qd = mul_u256_u128(&q, d);
            let sum = add(&qd, &U256::from_u128(r));
            assert_eq!(sum, p);
        }
    }

    fn mul_u256_u128(q: &U256, d: u128) -> U256 {
        let lo = q.checked_mul_u64(d as u64).unwrap();
        let hi = q.checked_mul_u64((d >> 64) as u64).unwrap();
        // hi << 64
        let shifted = U256([0, hi.0[0], hi.0[1], hi.0[2]]);
        add(&lo, &shifted)
    }

    fn add(a: &U256, b: &U256) -> U256 {
        let mut out = [0u64; 4];
        let mut carry = 0u128;
        for (i, slot) in out.iter_mut().enumerate() {
            let s = u128::from(a.0[i]) + u128::from(b.0[i]) + carry;
            *slot = s as u64;
            carry = s >> 64;
        }
        U256(out)
    }

    #[test]
    fn small_division_and_powers() {
        let mut ten_pow = U256::ONE;
        for _ in 0..77 {
            ten_pow = ten_pow.checked_mul_u64(10).unwrap();
        }
        assert!(ten_pow.checked_mul_u64(10).is_none());
        let (q, r) = ten_pow.div_rem_u64(1000);
        assert_eq!(r, 0);
        assert!(q < ten_pow);
        let big = U256([u64::MAX; 4]);
        let (q, r) = big.div_rem(&U256([0, 0, 0, 1 << 63]));
        assert_eq!(q.to_u64(), Some(1));
        assert_eq!(r, U256([u64::MAX, u64::MAX, u64::MAX, (1 << 63) - 1]));
    }
}
