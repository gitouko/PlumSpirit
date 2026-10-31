//! Spiderweb's formula language (`files/mathexpr.py` `formula`): a Python
//! expression subset with Python's own numbers — exact integers, floats,
//! and the complex numbers a negative number to a fractional power makes —
//! so a formula gives exactly what it gives in Spiderweb.
//!
//! Text: `×` → `*`, `^` → `**`. Allowed: numeric literals (also `True` /
//! `False`), the variable (`x`, or `t` for shapes), `pi`, `e`, other names
//! (numbers given when it is worked out, when `named`), `+ - * / // % **`,
//! unary `+ -`, calls of [`FUNCS`]. `**` with |exponent| > 64 is an error.
//!
//! Deviations (documented in SPIDERWEB.md): identifiers are ASCII or Unicode
//! letters without NFKC normalisation; expressions nested deeper than
//! [`MAX_DEPTH`] and integers past [`pyint::MAX_BITS`] are errors (Python
//! would crash or take minutes).

use std::cmp::Ordering;

use super::np::np_hypot;
use super::pyint::{Int, IntError};

/// Functions a formula may call.
pub const FUNCS: [&str; 22] = [
    "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "sqrt", "exp", "log",
    "ln", "log10", "log2", "abs", "min", "max", "floor", "ceil", "round", "pow",
];

/// Deepest expression accepted.
pub const MAX_DEPTH: usize = 900;
/// Python's tokenizer limit on nested brackets.
const MAX_PARENS: usize = 200;

const KEYWORDS: [&str; 35] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

fn is_func(s: &str) -> bool {
    FUNCS.contains(&s)
}

// ------------------------------------------------------------------ numbers

/// A Python number.
#[derive(Clone, Debug)]
pub enum Num {
    I(Int),
    F(f64),
    C(f64, f64),
}

/// Something Python would raise; the formula can't be worked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fail;

type R<T> = Result<T, Fail>;

impl From<IntError> for Fail {
    fn from(_: IntError) -> Fail {
        Fail
    }
}

fn to_f(n: &Num) -> R<f64> {
    match n {
        Num::I(i) => Ok(i.to_f64()?),
        Num::F(f) => Ok(*f),
        Num::C(..) => Err(Fail),
    }
}

fn to_c(n: &Num) -> R<(f64, f64)> {
    match n {
        Num::C(r, i) => Ok((*r, *i)),
        n => Ok((to_f(n)?, 0.0)),
    }
}

fn is_c(a: &Num, b: &Num) -> bool {
    matches!(a, Num::C(..)) || matches!(b, Num::C(..))
}

fn c_prod(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

/// CPython 3.13 `_Py_c_quot` (None: division by zero). (3.14 also recovers
/// infinities and zeros computed as nan+nanj; 3.13 does not.)
fn c_quot(a: (f64, f64), b: (f64, f64)) -> Option<(f64, f64)> {
    let (abs_r, abs_i) = (b.0.abs(), b.1.abs());
    let r = if abs_r >= abs_i {
        if abs_r == 0.0 {
            return None;
        }
        let ratio = b.1 / b.0;
        let denom = b.0 + b.1 * ratio;
        ((a.0 + a.1 * ratio) / denom, (a.1 - a.0 * ratio) / denom)
    } else if abs_i >= abs_r {
        let ratio = b.0 / b.1;
        let denom = b.0 * ratio + b.1;
        ((a.0 * ratio + a.1) / denom, (a.1 * ratio - a.0) / denom)
    } else {
        (f64::NAN, f64::NAN)
    };
    Some(r)
}

/// CPython `complex_pow`.
fn c_pow(a: (f64, f64), b: (f64, f64)) -> R<Num> {
    let p = if b.1 == 0.0 && b.0 == b.0.floor() && b.0.abs() <= 100.0 {
        let n = b.0 as i64;
        let powu = |n: i64| {
            let mut r = (1.0, 0.0);
            let mut p = a;
            let mut mask = 1i64;
            while mask > 0 && n >= mask {
                if n & mask != 0 {
                    r = c_prod(r, p);
                }
                mask <<= 1;
                p = c_prod(p, p);
            }
            r
        };
        if n > 0 {
            powu(n)
        } else {
            c_quot((1.0, 0.0), powu(-n)).ok_or(Fail)?
        }
    } else if b.0 == 0.0 && b.1 == 0.0 {
        (1.0, 0.0)
    } else if a.0 == 0.0 && a.1 == 0.0 {
        if b.1 != 0.0 || b.0 < 0.0 {
            return Err(Fail);
        }
        (0.0, 0.0)
    } else {
        let vabs = np_hypot(a.0, a.1);
        let mut len = vabs.powf(b.0);
        let at = a.1.atan2(a.0);
        let mut phase = at * b.0;
        if b.1 != 0.0 {
            len /= (at * b.1).exp();
            phase += b.1 * vabs.ln();
        }
        if phase.is_infinite() {
            return Err(Fail); // cos / sin domain error
        }
        (len * phase.cos(), len * phase.sin())
    };
    if p.0.is_infinite() || p.1.is_infinite() {
        return Err(Fail);
    }
    Ok(Num::C(p.0, p.1))
}

fn is_odd_float(x: f64) -> bool {
    x.abs() % 2.0 == 1.0
}

/// CPython `float_pow`.
fn f_pow(iv: f64, iw: f64) -> R<Num> {
    if iw == 0.0 {
        return Ok(Num::F(1.0));
    }
    if iv.is_nan() {
        return Ok(Num::F(iv));
    }
    if iw.is_nan() {
        return Ok(Num::F(if iv == 1.0 { 1.0 } else { iw }));
    }
    if iw.is_infinite() {
        let a = iv.abs();
        return Ok(Num::F(if a == 1.0 {
            1.0
        } else if (iw > 0.0) == (a > 1.0) {
            iw.abs()
        } else {
            0.0
        }));
    }
    if iv.is_infinite() {
        let odd = is_odd_float(iw);
        return Ok(Num::F(if iw > 0.0 {
            if odd { iv } else { iv.abs() }
        } else if odd {
            0.0f64.copysign(iv)
        } else {
            0.0
        }));
    }
    if iv == 0.0 {
        if iw < 0.0 {
            return Err(Fail);
        }
        return Ok(Num::F(if is_odd_float(iw) { iv } else { 0.0 }));
    }
    let mut iv = iv;
    let mut negate = false;
    if iv < 0.0 {
        if iw != iw.floor() {
            return c_pow((iv, 0.0), (iw, 0.0));
        }
        iv = -iv;
        negate = is_odd_float(iw);
    }
    if iv == 1.0 {
        return Ok(Num::F(if negate { -1.0 } else { 1.0 }));
    }
    let ix = iv.powf(iw);
    if ix.is_infinite() {
        return Err(Fail); // OverflowError
    }
    Ok(Num::F(if negate { -ix } else { ix }))
}

/// CPython float floor division / modulo.
fn f_divmod(vx: f64, wx: f64) -> R<(f64, f64)> {
    if wx == 0.0 {
        return Err(Fail);
    }
    let mut m = vx % wx;
    let mut div = (vx - m) / wx;
    if m != 0.0 {
        if (wx < 0.0) != (m < 0.0) {
            m += wx;
            div -= 1.0;
        }
    } else {
        m = 0.0f64.copysign(wx);
    }
    let floordiv = if div != 0.0 {
        let mut f = div.floor();
        if div - f > 0.5 {
            f += 1.0;
        }
        f
    } else {
        0.0f64.copysign(vx / wx)
    };
    Ok((floordiv, m))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
}

fn binop(op: Op, a: &Num, b: &Num) -> R<Num> {
    if let (Num::I(x), Num::I(y)) = (a, b) {
        return Ok(match op {
            Op::Add => Num::I(x.add(y)?),
            Op::Sub => Num::I(x.sub(y)?),
            Op::Mul => Num::I(x.mul(y)?),
            Op::Div => Num::F(x.true_div(y)?),
            Op::FloorDiv => Num::I(x.divmod(y)?.0),
            Op::Mod => Num::I(x.divmod(y)?.1),
            Op::Pow => {
                if y.is_neg() {
                    return f_pow(x.to_f64()?, y.to_f64()?);
                }
                Num::I(x.pow(y)?)
            }
        });
    }
    if is_c(a, b) {
        let (x, y) = (to_c(a)?, to_c(b)?);
        return match op {
            Op::Add => Ok(Num::C(x.0 + y.0, x.1 + y.1)),
            Op::Sub => Ok(Num::C(x.0 - y.0, x.1 - y.1)),
            Op::Mul => {
                let p = c_prod(x, y);
                Ok(Num::C(p.0, p.1))
            }
            Op::Div => {
                let q = c_quot(x, y).ok_or(Fail)?;
                Ok(Num::C(q.0, q.1))
            }
            Op::Pow => c_pow(x, y),
            Op::FloorDiv | Op::Mod => Err(Fail),
        };
    }
    let (x, y) = (to_f(a)?, to_f(b)?);
    Ok(match op {
        Op::Add => Num::F(x + y),
        Op::Sub => Num::F(x - y),
        Op::Mul => Num::F(x * y),
        Op::Div => {
            if y == 0.0 {
                return Err(Fail);
            }
            Num::F(x / y)
        }
        Op::FloorDiv => Num::F(f_divmod(x, y)?.0),
        Op::Mod => {
            if y == 0.0 {
                return Err(Fail);
            }
            let mut m = x % y;
            if m != 0.0 {
                if (y < 0.0) != (m < 0.0) {
                    m += y;
                }
            } else {
                m = 0.0f64.copysign(y);
            }
            Num::F(m)
        }
        Op::Pow => return f_pow(x, y),
    })
}

/// CPython `_Py_c_abs` (None: overflow).
fn c_abs(r: f64, i: f64) -> R<f64> {
    if !r.is_finite() || !i.is_finite() {
        if r.is_infinite() {
            return Ok(r.abs());
        }
        if i.is_infinite() {
            return Ok(i.abs());
        }
        return Ok(f64::NAN);
    }
    let v = np_hypot(r, i);
    if v.is_finite() { Ok(v) } else { Err(Fail) }
}

fn py_abs(n: &Num) -> R<Num> {
    Ok(match n {
        Num::I(i) => Num::I(i.abs()),
        Num::F(f) => Num::F(f.abs()),
        Num::C(r, i) => Num::F(c_abs(*r, *i)?),
    })
}

/// `a > b` (`gt`) or `a < b`, exact between ints and floats; complex fails.
fn compare(a: &Num, b: &Num) -> R<Option<Ordering>> {
    Ok(match (a, b) {
        (Num::C(..), _) | (_, Num::C(..)) => return Err(Fail),
        (Num::I(x), Num::I(y)) => Some(x.cmp(y)),
        (Num::I(x), Num::F(f)) => x.cmp_f64(*f),
        (Num::F(f), Num::I(y)) => y.cmp_f64(*f).map(Ordering::reverse),
        (Num::F(x), Num::F(y)) => x.partial_cmp(y),
    })
}

/// `math_1`: a libm function with Python's error rules.
fn math1(n: &Num, f: fn(f64) -> f64) -> R<Num> {
    let x = to_f(n)?;
    let r = f(x);
    if (r.is_nan() && !x.is_nan()) || (r.is_infinite() && x.is_finite()) {
        return Err(Fail);
    }
    Ok(Num::F(r))
}

/// `loghelper` with `m_log` / `m_log10` / `m_log2`.
fn log_of(n: &Num, f: fn(f64) -> f64) -> R<f64> {
    match n {
        Num::I(i) => {
            if i.is_neg() || i.is_zero() {
                return Err(Fail);
            }
            match i.to_f64() {
                Ok(x) => Ok(f(x)),
                Err(_) => {
                    let (m, e) = i.frexp();
                    Ok(f(m) + f(2.0) * e as f64)
                }
            }
        }
        Num::F(x) => {
            let x = *x;
            if x.is_nan() || x == f64::INFINITY {
                Ok(x)
            } else if x > 0.0 {
                Ok(f(x))
            } else {
                Err(Fail)
            }
        }
        Num::C(..) => Err(Fail),
    }
}

/// Python `round(x)` / `round(x, n)`.
fn py_round(args: &[Num]) -> R<Num> {
    match args {
        [Num::I(i)] => Ok(Num::I(i.clone())),
        [Num::F(f)] => Ok(Num::I(Int::from_f64(f.round_ties_even())?)),
        [x, Num::I(n)] => match x {
            Num::I(i) => {
                if !n.is_neg() {
                    return Ok(Num::I(i.clone()));
                }
                let k = n.abs();
                // 10^k > 2|i| → 0
                let kb = k.to_i64().unwrap_or(i64::MAX);
                if (kb as f64) * std::f64::consts::LOG2_10 > (i.bits() + 2) as f64 {
                    return Ok(Num::I(Int::zero()));
                }
                let p = Int::pow10(kb as u64)?;
                let (mut q, r) = i.divmod(&p)?;
                let two_r = r.add(&r)?;
                let c = two_r.cmp(&p);
                if c == Ordering::Greater || (c == Ordering::Equal && q.is_odd()) {
                    q = q.add(&Int::from_i64(1))?;
                }
                Ok(Num::I(q.mul(&p)?))
            }
            Num::F(x) => {
                let x = *x;
                if !x.is_finite() {
                    return Ok(Num::F(x));
                }
                let nd = n
                    .to_i64()
                    .unwrap_or(if n.is_neg() { i64::MIN } else { i64::MAX });
                if nd > 323 {
                    return Ok(Num::F(x));
                }
                if nd < -308 {
                    return Ok(Num::F(0.0 * x));
                }
                Ok(Num::F(double_round(x, nd)?))
            }
            Num::C(..) => Err(Fail),
        },
        _ => Err(Fail),
    }
}

/// CPython `double_round`: |x| rounded to `nd` decimals (exact value, ties
/// to even), read back as the nearest float, sign kept.
fn double_round(x: f64, nd: i64) -> R<f64> {
    if x == 0.0 {
        return Ok(x);
    }
    let a = x.abs();
    let bits = a.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64;
    let (m, e) = if exp == 0 {
        (bits & ((1u64 << 52) - 1), -1074i64)
    } else {
        ((bits & ((1u64 << 52) - 1)) | (1u64 << 52), exp - 1075)
    };
    // value = m * 2^e; want N = round(value * 10^nd)
    let mut num = Int::from_i64(m as i64);
    let mut den = Int::from_i64(1);
    let two = Int::from_i64(2);
    if e >= 0 {
        num = num.mul(&two.pow(&Int::from_i64(e))?)?;
    } else {
        den = two.pow(&Int::from_i64(-e))?;
    }
    if nd >= 0 {
        num = num.mul(&Int::pow10(nd as u64)?)?;
    } else {
        den = den.mul(&Int::pow10((-nd) as u64)?)?;
    }
    let (mut q, r) = num.divmod(&den)?;
    let c = r.add(&r)?.cmp(&den);
    if c == Ordering::Greater || (c == Ordering::Equal && q.is_odd()) {
        q = q.add(&Int::from_i64(1))?;
    }
    let text = format!("{}e{}", q.to_decimal(), -nd);
    let v: f64 = text.parse().map_err(|_| Fail)?;
    if v.is_infinite() {
        return Err(Fail);
    }
    Ok(v.copysign(x))
}

/// Python 3-argument `pow(a, b, m)`.
fn pow_mod(a: &Int, b: &Int, m: &Int) -> R<Int> {
    if m.is_zero() {
        return Err(Fail);
    }
    let neg_out = m.is_neg();
    let m = m.abs();
    if m.cmp(&Int::from_i64(1)) == Ordering::Equal {
        return Ok(Int::zero());
    }
    let work = b.bits() as f64 * (m.bits() as f64 / 32.0 + 1.0).powi(2);
    if work > 4e8 {
        return Err(Fail);
    }
    let mut base = a.divmod(&m)?.1;
    let mut e = b.clone();
    if e.is_neg() {
        base = invmod(&base, &m)?;
        e = e.neg();
    }
    let mut result = Int::from_i64(1);
    let two = Int::from_i64(2);
    while !e.is_zero() {
        let (q, r) = e.divmod(&two)?;
        if !r.is_zero() {
            result = result.mul(&base)?.divmod(&m)?.1;
        }
        e = q;
        if !e.is_zero() {
            base = base.mul(&base)?.divmod(&m)?.1;
        }
    }
    if neg_out && !result.is_zero() {
        result = result.sub(&m)?;
    }
    Ok(result)
}

fn invmod(a: &Int, m: &Int) -> R<Int> {
    let (mut r0, mut r1) = (m.clone(), a.clone());
    let (mut s0, mut s1) = (Int::zero(), Int::from_i64(1));
    while !r1.is_zero() {
        let (q, r) = r0.divmod(&r1)?;
        let s = s0.sub(&q.mul(&s1)?)?;
        r0 = r1;
        r1 = r;
        s0 = s1;
        s1 = s;
    }
    if r0.cmp(&Int::from_i64(1)) != Ordering::Equal {
        return Err(Fail);
    }
    Ok(s0.divmod(m)?.1)
}

fn call(name: &str, args: &[Num]) -> R<Num> {
    let one = |f: fn(f64) -> f64| -> R<Num> {
        match args {
            [a] => math1(a, f),
            _ => Err(Fail),
        }
    };
    match name {
        "sin" => one(f64::sin),
        "cos" => one(f64::cos),
        "tan" => one(f64::tan),
        "asin" => one(f64::asin),
        "acos" => one(f64::acos),
        "atan" => one(f64::atan),
        "sinh" => one(f64::sinh),
        "cosh" => one(f64::cosh),
        "tanh" => one(f64::tanh),
        "sqrt" => one(f64::sqrt),
        "exp" => one(f64::exp),
        "log" | "ln" => match args {
            [a] => Ok(Num::F(log_of(a, f64::ln)?)),
            [a, b] => {
                let num = log_of(a, f64::ln)?;
                let den = log_of(b, f64::ln)?;
                if den == 0.0 {
                    return Err(Fail);
                }
                Ok(Num::F(num / den))
            }
            _ => Err(Fail),
        },
        "log10" => match args {
            [a] => Ok(Num::F(log_of(a, f64::log10)?)),
            _ => Err(Fail),
        },
        "log2" => match args {
            [a] => Ok(Num::F(log_of(a, f64::log2)?)),
            _ => Err(Fail),
        },
        "abs" => match args {
            [a] => py_abs(a),
            _ => Err(Fail),
        },
        "min" | "max" => {
            if args.len() < 2 {
                return Err(Fail);
            }
            let want = if name == "max" {
                Ordering::Greater
            } else {
                Ordering::Less
            };
            let mut best = &args[0];
            for a in &args[1..] {
                if compare(a, best)? == Some(want) {
                    best = a;
                }
            }
            Ok(best.clone())
        }
        "floor" | "ceil" => match args {
            [Num::I(i)] => Ok(Num::I(i.clone())),
            [Num::F(f)] => Ok(Num::I(Int::from_f64(if name == "floor" {
                f.floor()
            } else {
                f.ceil()
            })?)),
            _ => Err(Fail),
        },
        "round" => py_round(args),
        "pow" => match args {
            [a, b] => binop(Op::Pow, a, b),
            [Num::I(a), Num::I(b), Num::I(m)] => Ok(Num::I(pow_mod(a, b, m)?)),
            _ => Err(Fail),
        },
        _ => Err(Fail),
    }
}

// ------------------------------------------------------------------ parsing

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Int(String, u32),
    Float(f64),
    Name(String),
    Op(&'static str),
}

fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic() || (!c.is_ascii() && c.is_alphabetic())
}

fn ident_char(c: char) -> bool {
    ident_start(c) || c.is_ascii_digit() || ('\u{ff10}'..='\u{ff19}').contains(&c)
}

/// The NFKC form Python gives identifiers, for the fullwidth forms of ASCII
/// (`ｘ` is `x`); other compatibility characters are kept as typed.
fn nfkc_name(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
            c => c,
        })
        .collect()
}

/// Python's tokenizer for the parts a formula can use. Err: SyntaxError
/// (or a token no formula accepts).
fn tokenize(text: &str) -> Result<Vec<Tok>, ()> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut level = 0usize;
    let mut ended = false; // a newline outside brackets: nothing may follow
    while i < c.len() {
        let ch = c[i];
        if ch == ' ' || ch == '\t' || ch == '\u{c}' {
            i += 1;
            continue;
        }
        if ch == '#' {
            while i < c.len() && c[i] != '\n' && c[i] != '\r' {
                i += 1;
            }
            continue;
        }
        if ch == '\\' {
            // line continuation
            match (c.get(i + 1), c.get(i + 2)) {
                (Some('\n'), _) => i += 2,
                (Some('\r'), Some('\n')) => i += 3,
                (Some('\r'), _) => i += 2,
                _ => return Err(()),
            }
            if i >= c.len() {
                return Err(());
            }
            continue;
        }
        if ch == '\n' || ch == '\r' {
            if level == 0 && !out.is_empty() {
                ended = true;
            }
            i += 1;
            continue;
        }
        if ended {
            return Err(());
        }
        if ch.is_ascii_digit() || (ch == '.' && c.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let (tok, n) = number(&c[i..])?;
            i += n;
            if c.get(i)
                .is_some_and(|&x| ident_char(x) || (!x.is_ascii() && !py_space(x)))
            {
                return Err(()); // invalid decimal literal
            }
            out.push(tok);
            continue;
        }
        if ident_start(ch) {
            let st = i;
            while i < c.len() && ident_char(c[i]) {
                i += 1;
            }
            if i < c.len() && !c[i].is_ascii() && !py_space(c[i]) {
                return Err(()); // a character no identifier may have
            }
            out.push(Tok::Name(nfkc_name(&c[st..i].iter().collect::<String>())));
            continue;
        }
        let two: String = c[i..(i + 2).min(c.len())].iter().collect();
        let op = match two.as_str() {
            "**" => Some("**"),
            "//" => Some("//"),
            _ => None,
        };
        if let Some(op) = op {
            // `**=` / `//=` are other tokens
            if c.get(i + 2) == Some(&'=') {
                return Err(());
            }
            out.push(Tok::Op(op));
            i += 2;
            continue;
        }
        let op = match ch {
            '+' => "+",
            '-' => "-",
            '*' => "*",
            '/' => "/",
            '%' => "%",
            ',' => ",",
            '(' => "(",
            ')' => ")",
            _ => return Err(()),
        };
        if matches!(op, "+" | "-" | "*" | "/" | "%") && c.get(i + 1) == Some(&'=') {
            return Err(());
        }
        if op == "(" {
            if level >= MAX_PARENS {
                return Err(());
            }
            level += 1;
        } else if op == ")" {
            if level == 0 {
                return Err(());
            }
            level -= 1;
        }
        out.push(Tok::Op(op));
        i += 1;
    }
    if level != 0 {
        return Err(());
    }
    Ok(out)
}

/// A digit run with single underscores between digits.
fn digits(c: &[char], ok: impl Fn(char) -> bool) -> Option<(String, usize)> {
    let mut s = String::new();
    let mut i = 0;
    if !c.first().is_some_and(|&x| ok(x)) {
        return None;
    }
    while i < c.len() {
        if ok(c[i]) {
            s.push(c[i]);
            i += 1;
        } else if c[i] == '_' && c.get(i + 1).is_some_and(|&x| ok(x)) {
            i += 1;
        } else {
            break;
        }
    }
    Some((s, i))
}

fn number(c: &[char]) -> Result<(Tok, usize), ()> {
    let dec = |x: char| x.is_ascii_digit();
    if c[0] == '0' && c.len() > 1 && matches!(c[1], 'x' | 'X' | 'o' | 'O' | 'b' | 'B') {
        let radix = match c[1] {
            'x' | 'X' => 16,
            'o' | 'O' => 8,
            _ => 2,
        };
        let mut i = 2;
        if c.get(i) == Some(&'_') {
            i += 1;
        }
        let (d, n) = digits(&c[i..], |x| x.is_digit(radix)).ok_or(())?;
        let i = i + n;
        if c.get(i).is_some_and(|x| x.is_ascii_digit() || *x == '_') {
            return Err(());
        }
        return Ok((Tok::Int(d, radix), i));
    }
    let mut i = 0;
    let mut text = String::new();
    let mut int_part = None;
    if c[0] != '.' {
        let (d, n) = digits(c, dec).ok_or(())?;
        text.push_str(&d);
        int_part = Some(d);
        i = n;
    }
    let mut is_float = false;
    if c.get(i) == Some(&'.') {
        is_float = true;
        text.push('.');
        i += 1;
        if let Some((d, n)) = digits(&c[i..], dec) {
            text.push_str(&d);
            i += n;
        } else if int_part.is_none() {
            return Err(());
        }
    }
    if matches!(c.get(i), Some('e' | 'E')) {
        let mut j = i + 1;
        let mut exp = String::from("e");
        if matches!(c.get(j), Some('+' | '-')) {
            exp.push(c[j]);
            j += 1;
        }
        let (d, n) = digits(&c[j..], dec).ok_or(())?;
        exp.push_str(&d);
        text.push_str(&exp);
        i = j + n;
        is_float = true;
    }
    if matches!(c.get(i), Some('j' | 'J')) {
        return Err(()); // imaginary: not a formula number
    }
    if c.get(i) == Some(&'_') {
        return Err(());
    }
    if is_float {
        return Ok((Tok::Float(text.parse().map_err(|_| ())?), i));
    }
    let d = int_part.ok_or(())?;
    if d.len() > 1 && d.starts_with('0') && d.chars().any(|x| x != '0') {
        return Err(()); // leading zeros
    }
    Ok((Tok::Int(d, 10), i))
}

#[derive(Clone, Debug)]
enum Node {
    Const(Num),
    Name(String),
    Bin(Op, Box<Node>, Box<Node>),
    Neg(Box<Node>),
    Pos(Box<Node>),
    Call(String, Vec<Node>),
}

/// Recursive descent over Python's expression grammar (the parts formulas
/// use; everything else is refused). Every function returns the node and
/// its height, refused past [`MAX_DEPTH`].
struct Parser {
    t: Vec<Tok>,
    i: usize,
}

type Parsed = Result<(Node, usize), ()>;

fn deeper(d: usize) -> Result<usize, ()> {
    if d > MAX_DEPTH { Err(()) } else { Ok(d) }
}

impl Parser {
    fn is_op(&self, op: &str) -> bool {
        matches!(self.t.get(self.i), Some(Tok::Op(o)) if *o == op)
    }

    fn take(&mut self, ops: &[(&str, Op)]) -> Option<Op> {
        for (s, op) in ops {
            if self.is_op(s) {
                self.i += 1;
                return Some(*op);
            }
        }
        None
    }

    fn expr(&mut self) -> Parsed {
        let (mut left, mut d) = self.term()?;
        while let Some(op) = self.take(&[("+", Op::Add), ("-", Op::Sub)]) {
            let (right, e) = self.term()?;
            d = deeper(d.max(e) + 1)?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok((left, d))
    }

    fn term(&mut self) -> Parsed {
        let (mut left, mut d) = self.factor()?;
        let ops = [
            ("*", Op::Mul),
            ("/", Op::Div),
            ("//", Op::FloorDiv),
            ("%", Op::Mod),
        ];
        while let Some(op) = self.take(&ops) {
            let (right, e) = self.factor()?;
            d = deeper(d.max(e) + 1)?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok((left, d))
    }

    fn factor(&mut self) -> Parsed {
        // unary chains are iterative (their nodes nest; the height counts)
        let mut signs = Vec::new();
        while let Some(op) = self.take(&[("-", Op::Sub), ("+", Op::Add)]) {
            signs.push(op);
            if signs.len() > MAX_DEPTH {
                return Err(());
            }
        }
        let (base, mut d) = self.primary()?;
        let mut n = base;
        if self.is_op("**") {
            self.i += 1;
            let (exp, e) = self.factor()?;
            d = deeper(d.max(e) + 1)?;
            n = Node::Bin(Op::Pow, Box::new(n), Box::new(exp));
        }
        while let Some(op) = signs.pop() {
            d = deeper(d + 1)?;
            n = if op == Op::Sub {
                Node::Neg(Box::new(n))
            } else {
                Node::Pos(Box::new(n))
            };
        }
        Ok((n, d))
    }

    fn primary(&mut self) -> Parsed {
        let (mut node, mut d) = self.atom()?;
        while self.is_op("(") {
            self.i += 1;
            let mut args = Vec::new();
            let mut h = d;
            while !self.is_op(")") {
                let (a, e) = self.expr()?;
                h = h.max(e);
                args.push(a);
                if self.is_op(",") {
                    self.i += 1;
                } else if !self.is_op(")") {
                    return Err(());
                }
            }
            self.i += 1;
            d = deeper(h + 1)?;
            node = match node {
                Node::Name(f) => Node::Call(f, args),
                _ => return Err(()), // a call of something else: refused
            };
        }
        Ok((node, d))
    }

    fn atom(&mut self) -> Parsed {
        let t = self.t.get(self.i).cloned().ok_or(())?;
        self.i += 1;
        Ok(match t {
            Tok::Int(d, radix) => (Node::Const(Num::I(Int::parse(&d, radix).ok_or(())?)), 1),
            Tok::Float(f) => (Node::Const(Num::F(f)), 1),
            Tok::Name(n) => match n.as_str() {
                "True" => (Node::Const(Num::I(Int::from_i64(1))), 1),
                "False" => (Node::Const(Num::I(Int::zero())), 1),
                k if KEYWORDS.contains(&k) => return Err(()),
                _ => (Node::Name(n), 1),
            },
            Tok::Op("(") => {
                // `()` and tuples are refused
                let e = self.expr()?;
                if !self.is_op(")") {
                    return Err(());
                }
                self.i += 1;
                e
            }
            _ => return Err(()),
        })
    }
}

// ------------------------------------------------------------------ the formula

#[derive(Clone, Debug)]
enum Code {
    Const(Num),
    Var,
    Known(f64),
    User(usize),
    Bin(Op, Box<Code>, Box<Code>),
    Neg(Box<Code>),
    Pos(Box<Code>),
    Call(&'static str, Vec<Code>),
}

/// A formula of one variable that can be worked out.
#[derive(Clone, Debug)]
pub struct Formula {
    code: Code,
    /// The other names in it, in the order they first appear.
    pub names: Vec<String>,
}

/// Why a formula can't be read (sentences for the window).
pub const TYPE_A_FORMULA: &str = "Type a formula.";
pub const CANT_READ: &str = "PlumSpirit can't read it (check the brackets and signs).";
pub const ONLY: &str = "Only numbers, x, + - * / ^ and functions like sin( ) work.";

fn check(n: &Node, named: bool, var: &str, names: &mut Vec<String>) -> Result<Code, String> {
    Ok(match n {
        Node::Const(v) => Code::Const(v.clone()),
        Node::Name(id) => {
            if id == var {
                Code::Var
            } else if id == "pi" {
                Code::Known(std::f64::consts::PI)
            } else if id == "e" {
                Code::Known(std::f64::consts::E)
            } else {
                if !named || is_func(id) {
                    return Err(format!(
                        "Unknown name \u{201c}{id}\u{201d} (use {var} for the position)."
                    ));
                }
                let k = match names.iter().position(|m| m == id) {
                    Some(k) => k,
                    None => {
                        names.push(id.clone());
                        names.len() - 1
                    }
                };
                Code::User(k)
            }
        }
        Node::Bin(op, a, b) => {
            let a = check(a, named, var, names)?;
            let b = check(b, named, var, names)?;
            Code::Bin(*op, Box::new(a), Box::new(b))
        }
        Node::Neg(a) => Code::Neg(Box::new(check(a, named, var, names)?)),
        Node::Pos(a) => Code::Pos(Box::new(check(a, named, var, names)?)),
        Node::Call(f, args) => {
            let Some(&name) = FUNCS.iter().find(|g| !g.is_empty() && *g == f) else {
                return Err(format!("Unknown function \u{201c}{f}\u{201d}."));
            };
            let mut v = Vec::with_capacity(args.len());
            for a in args {
                v.push(check(a, named, var, names)?);
            }
            Code::Call(name, v)
        }
    })
}

/// Python `str.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

impl Formula {
    /// `formula(text, named, var)`: Err is the sentence Spiderweb shows.
    pub fn parse(text: &str, named: bool, var: &str) -> Result<Formula, String> {
        let text = py_strip(text).replace('×', "*").replace('^', "**");
        if text.is_empty() {
            return Err(TYPE_A_FORMULA.into());
        }
        let toks = tokenize(&text).map_err(|()| CANT_READ.to_string())?;
        let mut p = Parser { t: toks, i: 0 };
        let (node, _) = p.expr().map_err(|()| CANT_READ.to_string())?;
        if p.i != p.t.len() {
            return Err(CANT_READ.into());
        }
        let mut names = Vec::new();
        let code = check(&node, named, var, &mut names)?;
        Ok(Formula { code, names })
    }

    /// The value at `x` with `values` for [`Formula::names`] (same order);
    /// Err when Python would raise (or the result is complex).
    pub fn eval(&self, x: f64, values: &[f64]) -> Result<f64, Fail> {
        match ev(&self.code, x, values)? {
            Num::C(..) => Err(Fail),
            v => to_f(&v),
        }
    }

    /// Values for [`Formula::names`] from `(name, value)` pairs (None: one
    /// is missing — Python's KeyError).
    pub fn bind(&self, vars: &[(String, f64)]) -> Option<Vec<f64>> {
        self.names
            .iter()
            .map(|n| vars.iter().find(|(m, _)| m == n).map(|(_, v)| *v))
            .collect()
    }
}

fn ev(c: &Code, x: f64, vals: &[f64]) -> R<Num> {
    Ok(match c {
        Code::Const(v) => v.clone(),
        Code::Var => Num::F(x),
        Code::Known(v) => Num::F(*v),
        Code::User(k) => Num::F(*vals.get(*k).ok_or(Fail)?),
        Code::Bin(op, a, b) => {
            let a = ev(a, x, vals)?;
            let b = ev(b, x, vals)?;
            if *op == Op::Pow {
                let big = match py_abs(&b)? {
                    Num::I(i) => i.cmp(&Int::from_i64(64)) == Ordering::Greater,
                    Num::F(f) => f > 64.0,
                    Num::C(..) => false,
                };
                if big {
                    return Err(Fail); // exponent too large
                }
            }
            binop(*op, &a, &b)?
        }
        Code::Neg(a) => match ev(a, x, vals)? {
            Num::I(i) => Num::I(i.neg()),
            Num::F(f) => Num::F(-f),
            Num::C(r, i) => Num::C(-r, -i),
        },
        Code::Pos(a) => ev(a, x, vals)?,
        Code::Call(f, args) => {
            let mut v = Vec::with_capacity(args.len());
            for a in args {
                v.push(ev(a, x, vals)?);
            }
            call(f, &v)?
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, x: f64) -> Option<f64> {
        let f = Formula::parse(text, true, "x").ok()?;
        let vals = vec![1.0; f.names.len()];
        f.eval(x, &vals).ok()
    }

    #[test]
    fn formulas_follow_python() {
        assert_eq!(at("x^2", 3.0), Some(9.0));
        assert_eq!(at("-2**2", 0.0), Some(-4.0));
        assert_eq!(at("2**-1", 0.0), Some(0.5));
        assert_eq!(at("7 // -2", 0.0), Some(-4.0));
        assert_eq!(at("-7.5 % 2", 0.0), Some(0.5));
        assert_eq!(at("round(2.5)", 0.0), Some(2.0));
        assert_eq!(at("round(2.675, 2)", 0.0), Some(2.67));
        assert_eq!(at("round(1250, -2)", 0.0), Some(1200.0));
        assert_eq!(at("abs((-1)**0.5)", 0.0), Some(1.0));
        assert_eq!(at("(-8)**(1/3)", 0.0), None); // complex
        assert_eq!(at("2**65", 0.0), None);
        assert_eq!(at("pow(2, 65)", 0.0), Some(36893488147419103232.0));
        assert_eq!(at("max(1, 2.0, 2)", 0.0), Some(2.0));
        assert_eq!(at("log(8, 2)", 0.0), Some(3.0));
        assert_eq!(at("sqrt(-1)", 0.0), None);
        assert_eq!(at("1/0", 0.0), None);
        assert_eq!(at("10**400", 0.0), None);
        assert_eq!(at("pow(10, 400) // pow(10, 399)", 0.0), Some(10.0));
        assert_eq!(at("x # comment", 4.0), Some(4.0));
        assert_eq!(at("(x\n + 1)", 4.0), Some(5.0));
        for bad in [
            "",
            "2x",
            "x[0]",
            "x.y",
            "lambda",
            "(x, 1)",
            "sin",
            "foo(x)",
            "0123",
            "1__0",
            "1j",
            "x if x else 1",
            "x\n+1",
            "()",
            "x(2)",
            "None",
            "'a'",
        ] {
            assert!(Formula::parse(bad, true, "x").is_err(), "{bad}");
        }
        let f = Formula::parse("height * sin(x * waves * 2 * pi) + height", true, "x").unwrap();
        assert_eq!(f.names, vec!["height", "waves"]);
        assert!(Formula::parse("height", false, "x").is_err());
    }
}
