//! Checked numeric text. Floats use their real representation; integers use
//! exact decimal digits, including exponent notation, before conversion.
use std::cmp::Ordering;
pub const MAX_TEXT: usize = 16 * 1024;
pub fn normalized(text: &str) -> Result<String, String> {
    if text.len() > MAX_TEXT {
        return Err("Numeric input is too long (16 KiB maximum).".into());
    }
    let s: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == '−' { '-' } else { c })
        .collect();
    if s.is_empty() {
        return Err("Enter a number or arithmetic expression.".into());
    }
    Ok(s)
}
fn parts(s: &str) -> Option<(String, i32)> {
    let (m, e) = s
        .split_once(['e', 'E'])
        .map_or(Some((s, 0)), |(m, e)| e.parse::<i32>().ok().map(|e| (m, e)))?;
    let sign = if m.starts_with('-') { "-" } else { "" };
    let m = m.strip_prefix(['-', '+']).unwrap_or(m);
    if m.matches('.').count() > 1
        || !m.chars().all(|c| c.is_ascii_digit() || c == '.')
        || !m.chars().any(|c| c.is_ascii_digit())
    {
        return None;
    }
    let frac = m.split_once('.').map_or(0, |(_, f)| f.len());
    let digits = m.replace('.', "");
    Some((
        format!("{sign}{digits}"),
        e.checked_sub(i32::try_from(frac).ok()?)?,
    ))
}
fn finite(v: f64, s: &str) -> Result<f64, String> {
    if !v.is_finite() {
        return Err("Enter a finite number; NaN, infinity and overflow are not supported.".into());
    }
    if v == 0.0
        && s.split(['e', 'E'])
            .next()
            .unwrap_or("")
            .chars()
            .any(|c| matches!(c, '1'..='9'))
    {
        return Err("This number underflows the floating-point representation.".into());
    }
    Ok(v)
}
pub fn float(text: &str) -> Result<f64, String> {
    let s = normalized(text)?;
    if let Ok(v) = s.parse::<f64>() {
        return finite(v, &s);
    }
    // Reject overflowing / underflowing literal operands before evaluating an expression.
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() || b[i] == b'.' {
            let a = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            if i < b.len() && matches!(b[i], b'e' | b'E') {
                i += 1;
                if i < b.len() && matches!(b[i], b'+' | b'-') {
                    i += 1;
                }
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if let Ok(v) = s[a..i].parse::<f64>() {
                finite(v, &s[a..i])?;
            }
        } else {
            i += 1;
        }
    }
    let e = plumspirit_engine::editor::expr::Expr::parse_vars(&s, &[]).map_err(|_| {
        "Enter a valid number or arithmetic expression; use a dot for decimals.".to_string()
    })?;
    let v = e.eval_vars(&[]).map_err(|_| {
        "The expression is outside its domain (for example, division by zero).".to_string()
    })?;
    finite(v, "")
}
pub fn integer(text: &str) -> Result<String, String> {
    let s = normalized(text)?;
    if let Some((digits, scale)) = parts(&s) {
        let negative = digits.starts_with('-');
        let mut d = digits
            .trim_start_matches('-')
            .trim_start_matches('0')
            .to_string();
        if d.is_empty() {
            return Ok("0".into());
        }
        if scale < 0 {
            let n = usize::try_from(scale.unsigned_abs())
                .map_err(|_| "Enter a whole number.".to_string())?;
            if n > d.len() || !d[d.len() - n..].bytes().all(|b| b == b'0') {
                return Err("Enter a whole number; fractional values are not rounded.".into());
            }
            d.truncate(d.len() - n);
        } else {
            let n = scale as usize;
            if d.len().saturating_add(n) > 40 {
                return Err("The integer exceeds the supported representation.".into());
            }
            d.extend(std::iter::repeat_n('0', n));
        }
        if d.len() > 40 {
            return Err("The integer exceeds the supported representation.".into());
        }
        return Ok(format!("{}{d}", if negative { "-" } else { "" }));
    }
    let v = float(&s)?;
    if v == 0.0 {
        return Ok("0".into());
    }
    if v.fract() != 0.0 {
        return Err("Enter a whole number; fractional values are not rounded.".into());
    }
    if v.abs() > 9_007_199_254_740_992.0 {
        return Err("Use whole-number digits or decimal exponent notation for exact large integers; arithmetic expressions use f64.".into());
    }
    Ok(format!("{v:.0}"))
}
pub fn compare_integer(a: &str, b: &str) -> Ordering {
    let an = a.starts_with('-');
    let bn = b.starts_with('-');
    if an != bn {
        return if an {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    let a = a.trim_start_matches('-');
    let b = b.trim_start_matches('-');
    let c = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
    if an { c.reverse() } else { c }
}
pub fn add_integer(value: &str, delta: i128, min: &str, max: &str) -> Result<String, String> {
    let n = value.parse::<i128>().map_err(|_| {
        "This integer is too large for step arithmetic; type an exact value instead.".to_string()
    })?;
    let mut s = n.saturating_add(delta).to_string();
    if compare_integer(&s, min).is_lt() {
        s = min.into();
    }
    if compare_integer(&s, max).is_gt() {
        s = max.into();
    }
    Ok(s)
}
pub fn add_decimal(a: f64, b: f64) -> f64 {
    let exact = || -> Option<f64> {
        let (a, ea) = parts(&a.to_string())?;
        let (b, eb) = parts(&b.to_string())?;
        let e = ea.min(eb);
        let scale = |n: String, x: i32| {
            n.parse::<i128>()
                .ok()?
                .checked_mul(10i128.checked_pow((x - e).try_into().ok()?)?)
        };
        let n = scale(a, ea)?.checked_add(scale(b, eb)?)?;
        format!("{n}e{e}").parse::<f64>().ok()
    };
    exact().filter(|v| v.is_finite()).unwrap_or(a + b)
}
pub fn display_float_text(s: String) -> String {
    if s.len() > 24
        && let Ok(v) = s.parse::<f64>()
    {
        format!("{v:e}")
    } else {
        s
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_precision_domains_and_exact_integers_are_checked() {
        for s in ["0.123456789012345", "1e-9", "-2.5000001", "1e200", "-0"] {
            assert_eq!(
                float(s).unwrap().to_bits(),
                s.parse::<f64>().unwrap().to_bits()
            );
        }
        for s in [
            "NaN", "inf", "1e400", "1e-400", "1e-400*2", "1/0", "sqrt(-1)", "2,5",
        ] {
            assert!(float(s).is_err(), "{s}");
        }
        assert_eq!(
            integer("18446744073709551615").unwrap(),
            u64::MAX.to_string()
        );
        assert_eq!(
            integer("18446744073709551615.000e0").unwrap(),
            u64::MAX.to_string()
        );
        assert_eq!(
            integer("−9223372036854775808").unwrap(),
            i64::MIN.to_string()
        );
        assert_eq!(integer("1.25e3").unwrap(), "1250");
        assert_eq!(integer("960*4").unwrap(), "3840");
        for s in ["1.1", "1e-9", "18446744073709551615.1"] {
            assert!(integer(s).is_err(), "{s}");
        }
        assert_eq!(
            add_integer(&u64::MAX.to_string(), -1, "0", &u64::MAX.to_string()).unwrap(),
            (u64::MAX - 1).to_string()
        );
        assert_eq!(add_decimal(0.2, 0.1), 0.3);
        assert_eq!(add_decimal(1e-12, 1e-12), 2e-12);
        assert!(float(&"0".repeat(MAX_TEXT + 1)).is_err());
    }
}
