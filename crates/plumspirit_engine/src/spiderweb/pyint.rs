//! Python `int` (arbitrary precision) as Spiderweb's formula language uses it:
//! exact `+ - * // % **`, correctly rounded conversion to float and true
//! division, exact comparison with floats. Std only.
//!
//! Integers are limited to [`MAX_BITS`] bits (Python has no limit, but a
//! formula reaching it would take Python seconds to minutes per sample);
//! beyond it the formula "can't be worked out" — a documented deviation.

use std::cmp::Ordering;

/// The largest integer a formula may build, in bits.
pub const MAX_BITS: u64 = 1 << 16;

/// Why an integer operation failed (Python's exception, roughly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntError {
    /// `ZeroDivisionError`.
    ZeroDivision,
    /// `OverflowError` (too large for a float).
    Overflow,
    /// Past [`MAX_BITS`] (PlumSpirit's limit).
    TooBig,
    /// `ValueError` (NaN to int).
    Value,
}

/// A Python integer: sign and magnitude (little-endian 32-bit limbs, no
/// leading zero limbs; zero is an empty magnitude, never negative).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Int {
    neg: bool,
    mag: Vec<u32>,
}

fn trim(v: &mut Vec<u32>) {
    while v.last() == Some(&0) {
        v.pop();
    }
}

fn mag_bits(a: &[u32]) -> u64 {
    match a.last() {
        None => 0,
        Some(&top) => (a.len() as u64 - 1) * 32 + u64::from(32 - top.leading_zeros()),
    }
}

fn mag_cmp(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

fn mag_add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u64;
    for (i, &x) in a.iter().enumerate() {
        let s = u64::from(x) + u64::from(*b.get(i).unwrap_or(&0)) + carry;
        out.push(s as u32);
        carry = s >> 32;
    }
    if carry != 0 {
        out.push(carry as u32);
    }
    out
}

/// a - b for a >= b.
fn mag_sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for (i, &x) in a.iter().enumerate() {
        let mut d = i64::from(x) - i64::from(*b.get(i).unwrap_or(&0)) - borrow;
        if d < 0 {
            d += 1 << 32;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(d as u32);
    }
    trim(&mut out);
    out
}

fn mag_mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u64;
        let x = u64::from(x);
        for (j, &y) in b.iter().enumerate() {
            let t = x * u64::from(y) + u64::from(out[i + j]) + carry;
            out[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let t = u64::from(out[k]) + carry;
            out[k] = t as u32;
            carry = t >> 32;
            k += 1;
        }
    }
    trim(&mut out);
    out
}

fn mag_shl(a: &[u32], n: u64) -> Vec<u32> {
    if a.is_empty() {
        return Vec::new();
    }
    let limbs = (n / 32) as usize;
    let bits = (n % 32) as u32;
    let mut out = vec![0u32; limbs];
    if bits == 0 {
        out.extend_from_slice(a);
    } else {
        let mut carry = 0u32;
        for &x in a {
            out.push((x << bits) | carry);
            carry = x >> (32 - bits);
        }
        if carry != 0 {
            out.push(carry);
        }
    }
    out
}

/// a >> n and whether any 1 bit was shifted out.
fn mag_shr(a: &[u32], n: u64) -> (Vec<u32>, bool) {
    let limbs = (n / 32) as usize;
    if limbs >= a.len() {
        return (Vec::new(), !a.is_empty());
    }
    let bits = (n % 32) as u32;
    let mut sticky = a[..limbs].iter().any(|&x| x != 0);
    let mut out = Vec::with_capacity(a.len() - limbs);
    if bits == 0 {
        out.extend_from_slice(&a[limbs..]);
    } else {
        sticky |= a[limbs] & ((1u32 << bits) - 1) != 0;
        for i in limbs..a.len() {
            let hi = if i + 1 < a.len() {
                a[i + 1] << (32 - bits)
            } else {
                0
            };
            out.push((a[i] >> bits) | hi);
        }
    }
    trim(&mut out);
    (out, sticky)
}

/// Quotient and remainder of magnitudes (b non-empty). Knuth's algorithm D
/// (as in Hacker's Delight `divmnu`).
fn mag_divmod(u: &[u32], v: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if mag_cmp(u, v) == Ordering::Less {
        return (Vec::new(), u.to_vec());
    }
    if v.len() == 1 {
        let d = u64::from(v[0]);
        let mut q = vec![0u32; u.len()];
        let mut r = 0u64;
        for i in (0..u.len()).rev() {
            let cur = (r << 32) | u64::from(u[i]);
            q[i] = (cur / d) as u32;
            r = cur % d;
        }
        trim(&mut q);
        let mut rv = vec![r as u32];
        trim(&mut rv);
        return (q, rv);
    }
    let n = v.len();
    let m = u.len() - n;
    let s = v[n - 1].leading_zeros();
    let vn: Vec<u32> = if s == 0 {
        v.to_vec()
    } else {
        (0..n)
            .map(|i| (v[i] << s) | if i > 0 { v[i - 1] >> (32 - s) } else { 0 })
            .collect()
    };
    let mut un = vec![0u32; u.len() + 1];
    if s == 0 {
        un[..u.len()].copy_from_slice(u);
    } else {
        un[u.len()] = u[u.len() - 1] >> (32 - s);
        for i in (1..u.len()).rev() {
            un[i] = (u[i] << s) | (u[i - 1] >> (32 - s));
        }
        un[0] = u[0] << s;
    }
    let b = 1u64 << 32;
    let mut q = vec![0u32; m + 1];
    for j in (0..=m).rev() {
        let num = (u64::from(un[j + n]) << 32) | u64::from(un[j + n - 1]);
        let mut qhat = num / u64::from(vn[n - 1]);
        let mut rhat = num - qhat * u64::from(vn[n - 1]);
        while qhat >= b || qhat * u64::from(vn[n - 2]) > (rhat << 32) + u64::from(un[j + n - 2]) {
            qhat -= 1;
            rhat += u64::from(vn[n - 1]);
            if rhat >= b {
                break;
            }
        }
        let mut k: i64 = 0;
        for i in 0..n {
            let p = qhat * u64::from(vn[i]);
            let t = i64::from(un[i + j]) - k - (p & 0xFFFF_FFFF) as i64;
            un[i + j] = t as u32;
            k = (p >> 32) as i64 - (t >> 32);
        }
        let t = i64::from(un[j + n]) - k;
        un[j + n] = t as u32;
        q[j] = qhat as u32;
        if t < 0 {
            q[j] = q[j].wrapping_sub(1);
            let mut c = 0u64;
            for i in 0..n {
                let t = u64::from(un[i + j]) + u64::from(vn[i]) + c;
                un[i + j] = t as u32;
                c = t >> 32;
            }
            un[j + n] = un[j + n].wrapping_add(c as u32);
        }
    }
    trim(&mut q);
    let mut r: Vec<u32> = if s == 0 {
        un[..n].to_vec()
    } else {
        (0..n)
            .map(|i| (un[i] >> s) | (un[i + 1] << (32 - s)))
            .collect()
    };
    trim(&mut r);
    (q, r)
}

/// 2^k as a float, -1074 <= k <= 1023.
fn pow2(k: i64) -> f64 {
    if k >= -1022 {
        f64::from_bits(((k + 1023) as u64) << 52)
    } else {
        f64::from_bits(1u64 << (k + 1074))
    }
}

/// x * 2^k, exact when the result is representable.
fn ldexp(mut x: f64, mut k: i64) -> f64 {
    while k > 1000 {
        x *= pow2(1000);
        k -= 1000;
        if x.is_infinite() {
            return x;
        }
    }
    while k < -1000 {
        x *= pow2(-1000);
        k += 1000;
    }
    x * pow2(k)
}

/// (magnitude + ε) · 2^scale correctly rounded (ties to even) to a float,
/// where ε is a tiny positive amount when `sticky` (bits below the
/// magnitude). None when it overflows. Callers give at least 55 bits when
/// `sticky` is set.
fn mag_to_f64(mag: &[u32], sticky: bool, scale: i64) -> Option<f64> {
    let bl = mag_bits(mag) as i64;
    if bl == 0 {
        return Some(0.0);
    }
    let e = bl - 1 + scale;
    if e > 1023 {
        return None;
    }
    let keep = if e >= -1022 { 53 } else { 53 - (-1022 - e) };
    if keep < 0 {
        return Some(0.0);
    }
    let drop = bl - keep;
    let v = if drop <= 0 {
        let mut m = 0u64;
        for (i, &x) in mag.iter().enumerate() {
            m |= u64::from(x) << (32 * i);
        }
        ldexp(m as f64, scale)
    } else {
        let (hi, _) = mag_shr(mag, drop as u64);
        let mut m = 0u64;
        for (i, &x) in hi.iter().enumerate() {
            m |= u64::from(x) << (32 * i);
        }
        // the dropped bits against half
        let half_bit = drop as u64 - 1;
        let limb = (half_bit / 32) as usize;
        let bit = (half_bit % 32) as u32;
        let half_set = mag.get(limb).is_some_and(|x| x >> bit & 1 == 1);
        let rest = {
            let mut any = mag[..limb].iter().any(|&x| x != 0);
            if bit > 0 {
                any |= mag[limb] & ((1u32 << bit) - 1) != 0;
            }
            any || sticky
        };
        if half_set && (rest || m & 1 == 1) {
            m += 1;
        }
        ldexp(m as f64, drop + scale)
    };
    if v.is_infinite() { None } else { Some(v) }
}

impl Int {
    pub fn zero() -> Int {
        Int {
            neg: false,
            mag: Vec::new(),
        }
    }

    pub fn from_i64(v: i64) -> Int {
        Int::from_u64(v.unsigned_abs(), v < 0)
    }

    fn from_u64(v: u64, neg: bool) -> Int {
        let mut mag = vec![v as u32, (v >> 32) as u32];
        trim(&mut mag);
        Int::of(neg, mag)
    }

    fn of(neg: bool, mag: Vec<u32>) -> Int {
        let neg = neg && !mag.is_empty();
        Int { neg, mag }
    }

    /// Digits in `radix` (2, 8, 10, 16), no sign, no underscores.
    pub fn parse(digits: &str, radix: u32) -> Option<Int> {
        let mut mag: Vec<u32> = Vec::new();
        for c in digits.chars() {
            let d = c.to_digit(radix)?;
            let mut carry = u64::from(d);
            for x in &mut mag {
                let t = u64::from(*x) * u64::from(radix) + carry;
                *x = t as u32;
                carry = t >> 32;
            }
            if carry != 0 {
                mag.push(carry as u32);
            }
            if mag_bits(&mag) > MAX_BITS + 64 {
                return None;
            }
        }
        trim(&mut mag);
        Some(Int::of(false, mag))
    }

    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    pub fn is_neg(&self) -> bool {
        self.neg
    }

    pub fn is_odd(&self) -> bool {
        self.mag.first().is_some_and(|x| x & 1 == 1)
    }

    /// Bit length of |self|.
    pub fn bits(&self) -> u64 {
        mag_bits(&self.mag)
    }

    pub fn to_i64(&self) -> Option<i64> {
        if self.bits() > 63 {
            return None;
        }
        let mut m = 0i64;
        for (i, &x) in self.mag.iter().enumerate() {
            m |= i64::from(x) << (32 * i);
        }
        Some(if self.neg { -m } else { m })
    }

    pub fn neg(&self) -> Int {
        Int::of(!self.neg, self.mag.clone())
    }

    pub fn abs(&self) -> Int {
        Int::of(false, self.mag.clone())
    }

    fn check(self) -> Result<Int, IntError> {
        if self.bits() > MAX_BITS {
            Err(IntError::TooBig)
        } else {
            Ok(self)
        }
    }

    pub fn add(&self, o: &Int) -> Result<Int, IntError> {
        let r = if self.neg == o.neg {
            Int::of(self.neg, mag_add(&self.mag, &o.mag))
        } else {
            match mag_cmp(&self.mag, &o.mag) {
                Ordering::Equal => Int::zero(),
                Ordering::Greater => Int::of(self.neg, mag_sub(&self.mag, &o.mag)),
                Ordering::Less => Int::of(o.neg, mag_sub(&o.mag, &self.mag)),
            }
        };
        r.check()
    }

    pub fn sub(&self, o: &Int) -> Result<Int, IntError> {
        self.add(&o.neg())
    }

    pub fn mul(&self, o: &Int) -> Result<Int, IntError> {
        if !self.is_zero() && !o.is_zero() && self.bits() + o.bits() - 1 > MAX_BITS {
            return Err(IntError::TooBig);
        }
        Int::of(self.neg != o.neg, mag_mul(&self.mag, &o.mag)).check()
    }

    /// Floor division and modulo (Python `divmod`).
    pub fn divmod(&self, o: &Int) -> Result<(Int, Int), IntError> {
        if o.is_zero() {
            return Err(IntError::ZeroDivision);
        }
        let (q, r) = mag_divmod(&self.mag, &o.mag);
        let mut q = Int::of(self.neg != o.neg, q);
        let mut r = Int::of(self.neg, r);
        if !r.is_zero() && r.neg != o.neg {
            q = q.sub(&Int::from_i64(1))?;
            r = r.add(o)?;
        }
        Ok((q, r))
    }

    /// self ** e for e >= 0.
    pub fn pow(&self, e: &Int) -> Result<Int, IntError> {
        debug_assert!(!e.neg);
        if e.is_zero() {
            return Ok(Int::from_i64(1));
        }
        if self.bits() <= 1 {
            // 0, 1, -1
            if self.is_zero() || !self.neg || !e.is_odd() {
                return Ok(if self.is_zero() {
                    Int::zero()
                } else {
                    Int::from_i64(1)
                });
            }
            return Ok(Int::from_i64(-1));
        }
        let Some(n) = e.to_i64() else {
            return Err(IntError::TooBig);
        };
        let n = n as u64;
        if (self.bits() - 1).saturating_mul(n) > MAX_BITS {
            return Err(IntError::TooBig);
        }
        let mut result = Int::from_i64(1);
        let mut base = self.clone();
        let mut k = n;
        loop {
            if k & 1 == 1 {
                result = result.mul(&base)?;
            }
            k >>= 1;
            if k == 0 {
                break;
            }
            base = base.mul(&base)?;
        }
        Ok(result)
    }

    /// `float(self)`: correctly rounded; Overflow past the float range.
    pub fn to_f64(&self) -> Result<f64, IntError> {
        let v = mag_to_f64(&self.mag, false, 0).ok_or(IntError::Overflow)?;
        Ok(if self.neg { -v } else { v })
    }

    /// `_PyLong_Frexp`: (m, e) with |self| ≈ m · 2^e, 0.5 <= m < 1 (self > 0).
    pub fn frexp(&self) -> (f64, i64) {
        let bl = self.bits() as i64;
        // m = self / 2^bl, rounded to 53 bits
        let m = mag_to_f64(&self.mag, false, -bl).unwrap_or(0.0);
        if m >= 1.0 { (m / 2.0, bl + 1) } else { (m, bl) }
    }

    /// The exact integer of an integral finite float.
    pub fn from_f64(f: f64) -> Result<Int, IntError> {
        if f.is_nan() {
            return Err(IntError::Value);
        }
        if f.is_infinite() {
            return Err(IntError::Overflow);
        }
        let f = f.trunc();
        if f == 0.0 {
            return Ok(Int::zero());
        }
        let bits = f.to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i64;
        let m = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
        let e = exp - 1075;
        let mag = Int::from_u64(m, false).mag;
        let mag = if e >= 0 {
            mag_shl(&mag, e as u64)
        } else {
            mag_shr(&mag, (-e) as u64).0
        };
        Ok(Int::of(f < 0.0, mag))
    }

    /// Python `self / o` (correctly rounded true division).
    pub fn true_div(&self, o: &Int) -> Result<f64, IntError> {
        if o.is_zero() {
            return Err(IntError::ZeroDivision);
        }
        let negative = self.neg != o.neg;
        if self.is_zero() {
            return Ok(if negative { -0.0 } else { 0.0 });
        }
        let (ba, bb) = (self.bits() as i64, o.bits() as i64);
        let v = if ba <= 53 && bb <= 53 {
            self.abs().to_f64()? / o.abs().to_f64()?
        } else {
            let shift = 55 + bb - ba;
            let (num, den) = if shift >= 0 {
                (mag_shl(&self.mag, shift as u64), o.mag.clone())
            } else {
                (self.mag.clone(), mag_shl(&o.mag, (-shift) as u64))
            };
            let (q, r) = mag_divmod(&num, &den);
            mag_to_f64(&q, !r.is_empty(), -shift).ok_or(IntError::Overflow)?
        };
        Ok(if negative { -v } else { v })
    }

    /// Exact comparison with a float (None: NaN).
    pub fn cmp_f64(&self, f: f64) -> Option<Ordering> {
        if f.is_nan() {
            return None;
        }
        if f.is_infinite() {
            return Some(if f > 0.0 {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        let fl = f.floor();
        let c = self.cmp(&Int::from_f64(fl).ok()?);
        Some(match c {
            Ordering::Equal if f > fl => Ordering::Less,
            c => c,
        })
    }

    /// self * 10^n (n >= 0).
    pub fn pow10(n: u64) -> Result<Int, IntError> {
        Int::from_i64(10).pow(&Int::from_i64(n as i64))
    }

    /// Decimal digits of |self|.
    pub fn to_decimal(&self) -> String {
        if self.is_zero() {
            return "0".into();
        }
        let mut parts = Vec::new();
        let mut m = self.mag.clone();
        while !m.is_empty() {
            let (q, r) = mag_divmod(&m, &[1_000_000_000]);
            parts.push(r.first().copied().unwrap_or(0));
            m = q;
        }
        let mut s = parts.pop().unwrap_or(0).to_string();
        while let Some(p) = parts.pop() {
            s.push_str(&format!("{p:09}"));
        }
        s
    }
}

impl Ord for Int {
    fn cmp(&self, o: &Int) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => mag_cmp(&self.mag, &o.mag),
            (true, true) => mag_cmp(&o.mag, &self.mag),
        }
    }
}

impl PartialOrd for Int {
    fn partial_cmp(&self, o: &Int) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(s: &str) -> Int {
        let (neg, d) = s.strip_prefix('-').map_or((false, s), |d| (true, d));
        let v = Int::parse(d, 10).unwrap();
        if neg { v.neg() } else { v }
    }

    #[test]
    fn integers_follow_python() {
        let a = int("123456789012345678901234567890");
        let b = int("-987654321987");
        let (q, r) = a.divmod(&b).unwrap();
        assert_eq!(q, int("-124999998748520314"));
        assert_eq!(r, int("-341931776028"));
        assert_eq!(
            a.mul(&b).unwrap(),
            int("-121932631246680382867668038286644871197430")
        );
        let big = int("2").pow(&int("80")).unwrap();
        let (q, r) = big.neg().divmod(&int("7")).unwrap();
        assert_eq!((q, r), (int("-172703688516375596386597"), int("3")));
        assert_eq!(big.divmod(&int("-7")).unwrap().1, int("-3"));
        let n = int("2").pow(&int("200")).unwrap().add(&int("1")).unwrap();
        let d = int("3").pow(&int("100")).unwrap();
        assert_eq!(n.true_div(&d), Ok(3117982410207.942));
        let p1075 = int("2").pow(&int("1075")).unwrap();
        assert_eq!(int("1").true_div(&p1075), Ok(0.0));
        assert_eq!(int("3").true_div(&p1075), Ok(1e-323));
        assert_eq!(
            int("2").pow(&int("64")).unwrap().to_decimal(),
            "18446744073709551616"
        );
        // float(10**40) == 1e40; 2**1024 overflows; (2**53 + 1) rounds to even
        assert_eq!(int("10").pow(&int("40")).unwrap().to_f64(), Ok(1e40));
        assert_eq!(
            int("2").pow(&int("1024")).unwrap().to_f64(),
            Err(IntError::Overflow)
        );
        assert_eq!(int("9007199254740993").to_f64(), Ok(9007199254740992.0));
        assert_eq!(int("9007199254740995").to_f64(), Ok(9007199254740996.0));
        // 10**30 / 3 == 3.333333333333333e+29; 1 / 2**1080 == 0.0 (underflow)
        assert_eq!(
            int("10").pow(&int("30")).unwrap().true_div(&int("3")),
            Ok(3.333333333333333e29)
        );
        let tiny = int("1").true_div(&int("2").pow(&int("1074")).unwrap());
        assert_eq!(tiny, Ok(5e-324));
        assert_eq!(
            Int::from_f64(1e20).unwrap().to_decimal(),
            "100000000000000000000"
        );
        assert_eq!(int("3").cmp_f64(2.5), Some(Ordering::Greater));
        assert_eq!(int("-3").cmp_f64(-2.5), Some(Ordering::Less));
        assert_eq!(int("2").cmp_f64(2.0), Some(Ordering::Equal));
        assert!(int("2").pow(&int("70000")).is_err());
    }
}
