//! NumPy / CPython numeric behaviours the port must reproduce bit for bit
//! (`SPIDERWEB_NOTES_SPEC.md` §0.3).

/// NumPy pairwise summation (`np.sum` of a contiguous float64 array).
pub fn pw_sum(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut r = 0.0;
        for &x in a {
            r += x;
        }
        r
    } else if n <= 128 {
        let mut r = [0.0f64; 8];
        r.copy_from_slice(&a[..8]);
        let end = n - n % 8;
        let mut i = 8;
        while i < end {
            for j in 0..8 {
                r[j] += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        for &x in &a[end..] {
            res += x;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pw_sum(&a[..n2]) + pw_sum(&a[n2..])
    }
}

/// Sequential sum (`np.cumsum`'s last value; a plain loop).
pub fn seq_sum(a: impl IntoIterator<Item = f64>) -> f64 {
    let mut r = 0.0;
    for x in a {
        r += x;
    }
    r
}

/// Python 3.12+ `sum()` of floats: Neumaier-compensated.
pub fn py_sum(a: impl IntoIterator<Item = f64>) -> f64 {
    let mut f = 0.0f64;
    let mut c = 0.0f64;
    for x in a {
        let t = f + x;
        if f.abs() >= x.abs() {
            c += (f - t) + x;
        } else {
            c += (x - t) + f;
        }
        f = t;
    }
    if c != 0.0 && c.is_finite() {
        f += c;
    }
    f
}

/// Python `math.hypot(x, y)`: CPython's `vector_norm` (scaled, lossless
/// squares with `fma`, compensated sum, one differential correction). It is
/// NOT the C library's `hypot` — they differ in about 0.1 % of inputs.
pub fn hypot(x: f64, y: f64) -> f64 {
    let (x, y) = (x.abs(), y.abs());
    let max = if y > x { y } else { x };
    vector_norm([x, y], max, x.is_nan() || y.is_nan())
}

/// Python `math.dist(a, b)` for two points.
#[inline]
pub fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    hypot(a[0] - b[0], a[1] - b[1])
}

/// `np.hypot` (the C library's `hypot`).
#[inline]
pub fn np_hypot(x: f64, y: f64) -> f64 {
    x.hypot(y)
}

fn vector_norm(mut v: [f64; 2], max: f64, found_nan: bool) -> f64 {
    if max.is_infinite() {
        return max;
    }
    if found_nan {
        return f64::NAN;
    }
    if max == 0.0 {
        return max;
    }
    let max_e = frexp_exp(max);
    if max_e < -1023 {
        // Subnormals: scale to normals first.
        for x in &mut v {
            *x /= f64::MIN_POSITIVE;
        }
        return f64::MIN_POSITIVE * vector_norm(v, max / f64::MIN_POSITIVE, found_nan);
    }
    let scale = ldexp1(-max_e);
    let (mut csum, mut frac1, mut frac2) = (1.0f64, 0.0f64, 0.0f64);
    for x in v {
        let x = x * scale;
        let (hi, lo) = dl_mul(x, x);
        let (shi, slo) = dl_fast_sum(csum, hi);
        csum = shi;
        frac1 += lo;
        frac2 += slo;
    }
    let mut h = (csum - 1.0 + (frac1 + frac2)).sqrt();
    let (hi, lo) = dl_mul(-h, h);
    let (shi, slo) = dl_fast_sum(csum, hi);
    csum = shi;
    frac1 += lo;
    frac2 += slo;
    let x = csum - 1.0 + (frac1 + frac2);
    h += x / (2.0 * h);
    h / scale
}

#[inline]
fn dl_mul(x: f64, y: f64) -> (f64, f64) {
    let z = x * y;
    (z, x.mul_add(y, -z))
}

#[inline]
fn dl_fast_sum(a: f64, b: f64) -> (f64, f64) {
    let x = a + b;
    (x, (a - x) + b)
}

/// The exponent `frexp` gives (`x = m * 2^e`, `0.5 <= m < 1`), x finite > 0.
fn frexp_exp(x: f64) -> i32 {
    let bits = x.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i32;
    if e == 0 {
        // subnormal: x = m * 2^-1074
        let m = bits & ((1u64 << 52) - 1);
        (64 - m.leading_zeros() as i32) - 1074
    } else {
        e - 1022
    }
}

/// `ldexp(1.0, e)` for `-1022 <= e <= 1023`... and the subnormal range.
fn ldexp1(e: i32) -> f64 {
    if e >= -1022 {
        f64::from_bits(((e + 1023) as u64) << 52)
    } else {
        f64::from_bits(1u64 << (e + 1074))
    }
}

/// `np.interp(x, xp, fp)` (xp ascending; duplicates allowed).
pub fn interp(x: f64, xp: &[f64], fp: &[f64]) -> f64 {
    let n = xp.len();
    if n == 0 {
        return f64::NAN;
    }
    if x.is_nan() {
        return x;
    }
    if x < xp[0] {
        return fp[0];
    }
    if x >= xp[n - 1] {
        return fp[n - 1];
    }
    // bisect_right - 1: the last j with xp[j] <= x.
    let j = xp.partition_point(|&v| v <= x) - 1;
    if x == xp[j] {
        return fp[j];
    }
    let slope = (fp[j + 1] - fp[j]) / (xp[j + 1] - xp[j]);
    let mut r = slope * (x - xp[j]) + fp[j];
    if r.is_nan() {
        r = slope * (x - xp[j + 1]) + fp[j + 1];
        if r.is_nan() && fp[j] == fp[j + 1] {
            r = fp[j];
        }
    }
    r
}

/// `np.linspace(a, b, n)`.
pub fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![a];
    }
    let step = (b - a) / (n - 1) as f64;
    let mut v: Vec<f64> = (0..n).map(|i| i as f64 * step + a).collect();
    v[n - 1] = b;
    v
}

/// Python `round(x, n)` (correctly rounded decimal, ties to even).
pub fn pyround(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.n$}").parse::<f64>().unwrap_or(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairwise_sum_and_interp_follow_numpy() {
        // np.sum(np.arange(1, 1000) * 0.1 + np.sin(np.arange(1, 1000))): pairwise
        // 49949.987090093535, sequential 49949.98709009354.
        let v: Vec<f64> = (1..1000)
            .map(|i| i as f64 * 0.1 + (i as f64).sin())
            .collect();
        assert_eq!(pw_sum(&v), 49949.987090093535);
        // Python 3.12+ sum() is compensated: 49949.98709009354.
        assert_eq!(py_sum(v.iter().copied()), 49949.98709009354);
        assert_eq!(seq_sum(v.iter().copied()), 49949.98709009358);
        assert_eq!(
            interp(1.5, &[0.0, 1.0, 1.0, 2.0], &[0.0, 1.0, 5.0, 6.0]),
            5.5
        );
        assert_eq!(
            interp(1.0, &[0.0, 1.0, 1.0, 2.0], &[0.0, 1.0, 5.0, 6.0]),
            5.0
        );
        assert_eq!(linspace(0.0, 1.0, 3), vec![0.0, 0.5, 1.0]);
        assert_eq!(pyround(2.675, 2), 2.67);
        assert_eq!(pyround(0.125, 2), 0.12);
    }
}
