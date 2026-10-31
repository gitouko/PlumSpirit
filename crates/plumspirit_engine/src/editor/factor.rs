//! Exact scale factors typed by the user.
//!
//! A factor such as `12.75`, `-1`, `1e6` or `0.333` is parsed as an exact decimal
//! rational `m × 10^e` (up to 38 significant digits) — never as a binary float — so
//! results do not depend on floating-point rounding (in binary, 0.3 × 5 is
//! 1.4999999999999998, which would round differently from the exact 1.5).
//!
//! There is **no magnitude cap**: any finite decimal is accepted. Whether the result
//! is representable (fits in MIDI ticks, stays within pitch 0..=127) is decided per
//! operation by exact arithmetic, and impossible requests are rejected as a whole.
//!
//! # Rounding rule
//!
//! Scaled values are rounded to the nearest integer; exact halves are rounded **away
//! from the pivot**. For Scale X the pivot is the earliest selected start, so this is
//! "round half up" on the distance from the pivot.

use crate::wide::U256;
use std::fmt;

/// Maximum number of significant decimal digits accepted.
pub const MAX_SIGNIFICANT_DIGITS: usize = 38;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactorError {
    Empty,
    NotANumber(String),
    TooManyDigits,
}

impl fmt::Display for FactorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FactorError::Empty => write!(f, "Enter a scale factor, for example 2 or 0.5."),
            FactorError::NotANumber(s) => write!(
                f,
                "\"{s}\" is not a number. Use digits with an optional sign, decimal point and exponent (for example -1, 12.75 or 1e6)."
            ),
            FactorError::TooManyDigits => write!(
                f,
                "The factor has more than {MAX_SIGNIFICANT_DIGITS} significant digits."
            ),
        }
    }
}

impl std::error::Error for FactorError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Magnitude {
    Zero,
    /// Exact `num / den`, reduced by common factors of 2 and 5.
    Ratio {
        num: U256,
        den: U256,
    },
    /// At least 2^256: any non-zero distance overflows every MIDI range.
    Huge,
    /// Below 10^-39: every distance a `u64` can hold rounds to zero.
    Tiny,
}

/// An exactly represented scale factor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactFactor {
    negative: bool,
    mag: Magnitude,
    text: String,
}

enum Rounding {
    /// round(x), ties up
    HalfUp,
    /// floor(x)
    Floor,
}

impl ExactFactor {
    /// Parses user input such as `2`, `0.5`, `-1`, `+3.25`, `.5`, `12.`, `1e6`,
    /// `2.5E-3`. Surrounding whitespace is ignored. `inf`, `NaN`, fractions and
    /// empty input are rejected.
    pub fn parse(input: &str) -> Result<Self, FactorError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(FactorError::Empty);
        }
        let bad = || FactorError::NotANumber(s.to_owned());
        let bytes = s.as_bytes();
        let mut i = 0;
        let mut negative = false;
        if bytes[0] == b'+' || bytes[0] == b'-' {
            negative = bytes[0] == b'-';
            i = 1;
        }
        let mut digits: Vec<u8> = Vec::new();
        let mut frac_len: i64 = 0;
        let mut seen_digit = false;
        let mut seen_dot = false;
        while i < bytes.len() {
            let c = bytes[i];
            if c.is_ascii_digit() {
                seen_digit = true;
                digits.push(c - b'0');
                if seen_dot {
                    frac_len += 1;
                }
            } else if c == b'.' && !seen_dot {
                seen_dot = true;
            } else {
                break;
            }
            i += 1;
        }
        if !seen_digit {
            return Err(bad());
        }
        let mut exp: i64 = 0;
        if i < bytes.len() {
            if bytes[i] != b'e' && bytes[i] != b'E' {
                return Err(bad());
            }
            i += 1;
            let mut exp_neg = false;
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                exp_neg = bytes[i] == b'-';
                i += 1;
            }
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                // Saturate absurd exponents; anything beyond ±10^6 is Huge/Tiny anyway.
                exp = (exp * 10 + i64::from(bytes[i] - b'0')).min(1_000_000);
                i += 1;
            }
            if i == start || i != bytes.len() {
                return Err(bad());
            }
            if exp_neg {
                exp = -exp;
            }
        }
        // Normalize: strip leading zeros, move trailing zeros into the exponent.
        let first_nz = digits.iter().position(|&d| d != 0);
        let Some(first_nz) = first_nz else {
            return Ok(ExactFactor {
                negative: false,
                mag: Magnitude::Zero,
                text: s.to_owned(),
            });
        };
        let mut sig: &[u8] = &digits[first_nz..];
        let mut e = exp - frac_len;
        while let Some((&0, rest)) = sig.split_last() {
            sig = rest;
            e += 1;
        }
        if sig.len() > MAX_SIGNIFICANT_DIGITS {
            return Err(FactorError::TooManyDigits);
        }
        let mut m: u128 = 0;
        for &d in sig {
            m = m * 10 + u128::from(d);
        }
        let mag = if e >= 0 {
            let mut num = U256::from_u128(m);
            let mut huge = false;
            for _ in 0..e {
                match num.checked_mul_u64(10) {
                    Some(n) => num = n,
                    None => {
                        huge = true;
                        break;
                    }
                }
            }
            if huge {
                Magnitude::Huge
            } else {
                Magnitude::Ratio {
                    num,
                    den: U256::ONE,
                }
            }
        } else {
            let mut den = U256::ONE;
            let mut tiny = false;
            for _ in 0..(-e) {
                match den.checked_mul_u64(10) {
                    Some(d) => den = d,
                    None => {
                        tiny = true;
                        break;
                    }
                }
            }
            if tiny {
                Magnitude::Tiny
            } else {
                let mut num = U256::from_u128(m);
                for p in [2u64, 5] {
                    loop {
                        let (qn, rn) = num.div_rem_u64(p);
                        let (qd, rd) = den.div_rem_u64(p);
                        if rn == 0 && rd == 0 {
                            num = qn;
                            den = qd;
                        } else {
                            break;
                        }
                    }
                }
                Magnitude::Ratio { num, den }
            }
        };
        Ok(ExactFactor {
            negative,
            mag,
            text: s.to_owned(),
        })
    }

    /// The text the user entered (trimmed).
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_negative(&self) -> bool {
        self.negative && self.mag != Magnitude::Zero
    }

    pub fn is_zero(&self) -> bool {
        self.mag == Magnitude::Zero
    }

    /// Exactly +1.
    pub fn is_one(&self) -> bool {
        !self.negative
            && matches!(self.mag, Magnitude::Ratio { num, den } if num == U256::ONE && den == U256::ONE)
    }

    /// `|factor| < 1`: the scale brings notes closer together (a shrink).
    pub fn shrinks(&self) -> bool {
        match self.mag {
            Magnitude::Zero | Magnitude::Tiny => true,
            Magnitude::Huge => false,
            Magnitude::Ratio { num, den } => num < den,
        }
    }

    /// Exactly -1.
    pub fn is_minus_one(&self) -> bool {
        self.negative
            && matches!(self.mag, Magnitude::Ratio { num, den } if num == U256::ONE && den == U256::ONE)
    }

    /// `round(distance × |factor|)` with ties rounded up (away from the pivot), or
    /// `None` if the result does not fit in a `u64`.
    pub fn scale_round(&self, distance: u64) -> Option<u64> {
        self.apply(distance, 1, Rounding::HalfUp)
    }

    /// `floor(distance × |factor| / 2)`, or `None` if it does not fit in a `u64`.
    pub fn scale_half_floor(&self, distance: u64) -> Option<u64> {
        self.apply(distance, 2, Rounding::Floor)
    }

    fn apply(&self, distance: u64, extra_den: u64, rounding: Rounding) -> Option<u64> {
        if distance == 0 {
            return Some(0);
        }
        match self.mag {
            Magnitude::Zero | Magnitude::Tiny => Some(0),
            Magnitude::Huge => None,
            Magnitude::Ratio { num, den } => {
                let den = den.checked_mul_u64(extra_den)?;
                // Fast path: 64-bit numerator and denominator -> 128-bit arithmetic.
                if let (Some(n), Some(d)) = (num.to_u64(), den.to_u64()) {
                    let prod = u128::from(distance) * u128::from(n);
                    let d = u128::from(d);
                    let q = prod / d;
                    let r = prod % d;
                    let q = match rounding {
                        Rounding::HalfUp if r >= d - r => q + 1,
                        _ => q,
                    };
                    return u64::try_from(q).ok();
                }
                // Slow path: exact 256-bit arithmetic. If the numerator itself
                // exceeds 128 bits the denominator is 1 (non-negative exponent), so
                // the result exceeds u64 for any non-zero distance.
                let n = num.to_u128()?;
                let prod = U256::mul_u64_u128(distance, n);
                let (q, r) = prod.div_rem(&den);
                let q = match rounding {
                    Rounding::HalfUp if r >= den.sub(&r) => {
                        let q1 = q.to_u64()?;
                        q1.checked_add(1)?
                    }
                    _ => q.to_u64()?,
                };
                Some(q)
            }
        }
    }

    /// Approximate value for display only (never used for editing math).
    pub fn approx_f64(&self) -> f64 {
        let v = self.text.parse::<f64>().unwrap_or(f64::NAN);
        if v.is_finite() {
            v
        } else if self.negative {
            f64::MIN
        } else {
            f64::MAX
        }
    }
}

impl fmt::Display for ExactFactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ExactFactor {
        ExactFactor::parse(s).unwrap()
    }

    #[test]
    fn parses_accepted_forms() {
        for s in [
            "0.5", "2", "12.75", "-1", "10000", "1e6", "+3", ".5", "2.", "1E-3", " 7 ", "-0", "0e5",
        ] {
            assert!(ExactFactor::parse(s).is_ok(), "{s}");
        }
        for s in [
            "", " ", "abc", "1/2", "inf", "NaN", "1e", "e5", "--1", "1.2.3", "1e5x", "0x10", ".",
        ] {
            assert!(ExactFactor::parse(s).is_err(), "{s}");
        }
        assert!(p("1").is_one());
        assert!(p("1.000").is_one());
        assert!(p("10e-1").is_one());
        assert!(p("-1").is_minus_one());
        assert!(p("-0").is_zero());
        assert!(!p("-0").is_negative());
        assert!(p("-2").is_negative());
        assert_eq!(
            ExactFactor::parse("1.234567890123456789012345678901234567891"),
            Err(FactorError::TooManyDigits)
        );
        // Trailing zeros are not significant.
        assert!(ExactFactor::parse("1000000000000000000000000000000000000000000000").is_ok());
    }

    #[test]
    fn exact_rounding_where_floats_fail() {
        // 0.3 * 5 = 1.5 exactly -> rounds up to 2 (f64 would give 1.4999999999999998).
        assert_eq!(p("0.3").scale_round(5), Some(2));
        assert_eq!(p("0.5").scale_round(3), Some(2)); // 1.5 -> 2
        assert_eq!(p("0.5").scale_round(1), Some(1)); // 0.5 -> 1
        assert_eq!(p("0.25").scale_round(1), Some(0));
        assert_eq!(p("2").scale_round(u64::MAX / 2), Some(u64::MAX - 1));
        assert_eq!(p("2").scale_round(u64::MAX / 2 + 1), None);
        assert_eq!(p("1e6").scale_round(1000), Some(1_000_000_000));
        assert_eq!(p("1e30").scale_round(1), None);
        assert_eq!(p("1e30").scale_round(0), Some(0));
        assert_eq!(p("1e-100").scale_round(u64::MAX), Some(0));
        assert_eq!(p("1e100000000").scale_round(1), None);
        assert_eq!(p("12.75").scale_round(4), Some(51));
        assert_eq!(p("0.5").scale_half_floor(3), Some(0)); // 0.75
        assert_eq!(p("3").scale_half_floor(3), Some(4)); // 4.5
        // Slow path (many digits): 0.1234567890123456789012345 * 8 = 0.98765... -> 1
        assert_eq!(p("0.1234567890123456789012345").scale_round(8), Some(1));
        assert_eq!(
            p("0.1234567890123456789012345").scale_round(10_000_000_000_000_000_000),
            Some(1_234_567_890_123_456_789)
        );
    }
}
