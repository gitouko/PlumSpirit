//! Custom curve functions: a small, safe mathematical expression language.
//!
//! A custom curve is a formula in one variable, `t`, that goes from 0 at the
//! start anchor to 1 at the end anchor. Its value is read as a normalised
//! position between the two anchor values (0 = the start value, 1 = the end
//! value; see `editor::curves`).
//!
//! The language is **only arithmetic**: numbers, `t`, the constants `pi` and
//! `e`, the operators `+ - * / ^` (power, right-associative, binding tighter
//! than a leading minus: `-t^2` is `-(t^2)`), `%` (floored modulo: the result
//! has the divisor's sign, `-1 % 16` is 15, `x % 1` the fraction) and `//`
//! (floor division) binding like `*` and `/`, parentheses, and the functions
//! `abs min max sqrt sin cos tan exp ln log pow clamp floor ceil round trunc
//! sign frac mod fmod asin acos atan atan2 hypot`
//! (`log` is base 10, `ln` natural; `min`/`max` take two or more arguments,
//! `pow(x, y)`, `clamp(x, lo, hi)`, `mod(x, y)` = `x % y`, `fmod(x, y)` the
//! remainder with the sign of `x`, `frac(x)` = `x - floor(x)`, `round` half
//! away from zero, `atan(y, x)` = `atan2(y, x)`, `hypot(x, y)`). Lua's
//! `math.` names are accepted as aliases (`math.sin(t)`, `math.pi`,
//! `math.floor`, `math.fmod`; `math.log(x)` is the natural log and
//! `math.log(x, b)` the base-`b` log, as in Lua); `math.huge` (infinity) and
//! `math.random` are refused (use a seeded variable such as `rnd` where a
//! tool offers one). There are no variables besides `t`, no
//! assignments, loops, strings, files, commands or calls to anything outside
//! this list: an expression is parsed into a small tree, and evaluating it can
//! only compute a number. Anything else is refused with a plain message.
//!
//! Resource limits: at most [`MAX_EXPR_LEN`] characters, [`MAX_NODES`] tree
//! nodes and [`MAX_DEPTH`] levels of nesting, so parsing and every evaluation
//! take bounded time and memory. A result that is not a finite number (NaN,
//! infinity — e.g. `sqrt(-1)`, `1/0`, `ln(0)`) is an error for that `t`, never
//! a silently changed value.

use std::fmt;

/// Longest expression, in characters.
pub const MAX_EXPR_LEN: usize = 1_000;
/// Most tree nodes (numbers, operators, calls) in one expression.
pub const MAX_NODES: usize = 512;
/// Deepest nesting of parentheses, calls and operators.
pub const MAX_DEPTH: usize = 64;

/// The functions an expression may call (each also as `math.<name>`;
/// `math.log` is Lua's natural / base-`b` log).
pub const FUNCTIONS: [&str; 25] = [
    "abs", "min", "max", "sqrt", "sin", "cos", "tan", "exp", "ln", "log", "pow", "clamp", "floor",
    "ceil", "round", "trunc", "sign", "frac", "mod", "fmod", "asin", "acos", "atan", "atan2",
    "hypot",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Func {
    Abs,
    Min,
    Max,
    Sqrt,
    Sin,
    Cos,
    Tan,
    Exp,
    Ln,
    Log,
    Pow,
    Clamp,
    Floor,
    Ceil,
    Round,
    Trunc,
    Sign,
    Frac,
    Mod,
    Fmod,
    Asin,
    Acos,
    Atan,
    Atan2,
    Hypot,
    /// Lua's `math.log(x [, base])`: natural, or base `b`.
    MathLog,
}

impl Func {
    fn from_name(n: &str) -> Option<Func> {
        Some(match n {
            "abs" => Func::Abs,
            "min" => Func::Min,
            "max" => Func::Max,
            "sqrt" => Func::Sqrt,
            "sin" => Func::Sin,
            "cos" => Func::Cos,
            "tan" => Func::Tan,
            "exp" => Func::Exp,
            "ln" => Func::Ln,
            "log" => Func::Log,
            "pow" => Func::Pow,
            "clamp" => Func::Clamp,
            "floor" => Func::Floor,
            "ceil" => Func::Ceil,
            "round" => Func::Round,
            "trunc" => Func::Trunc,
            "sign" => Func::Sign,
            "frac" => Func::Frac,
            "mod" => Func::Mod,
            "fmod" => Func::Fmod,
            "asin" => Func::Asin,
            "acos" => Func::Acos,
            "atan" => Func::Atan,
            "atan2" => Func::Atan2,
            "hypot" => Func::Hypot,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Func::Abs => "abs",
            Func::Min => "min",
            Func::Max => "max",
            Func::Sqrt => "sqrt",
            Func::Sin => "sin",
            Func::Cos => "cos",
            Func::Tan => "tan",
            Func::Exp => "exp",
            Func::Ln => "ln",
            Func::Log => "log",
            Func::Pow => "pow",
            Func::Clamp => "clamp",
            Func::Floor => "floor",
            Func::Ceil => "ceil",
            Func::Round => "round",
            Func::Trunc => "trunc",
            Func::Sign => "sign",
            Func::Frac => "frac",
            Func::Mod => "mod",
            Func::Fmod => "fmod",
            Func::Asin => "asin",
            Func::Acos => "acos",
            Func::Atan => "atan",
            Func::Atan2 => "atan2",
            Func::Hypot => "hypot",
            Func::MathLog => "math.log",
        }
    }

    /// Allowed argument counts (min, max).
    fn arity(self) -> (usize, usize) {
        match self {
            Func::Min | Func::Max => (2, 16),
            Func::Pow | Func::Mod | Func::Fmod | Func::Atan2 | Func::Hypot => (2, 2),
            Func::Atan | Func::MathLog => (1, 2),
            Func::Clamp => (3, 3),
            _ => (1, 1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    /// `%`: floored modulo (Lua's), the divisor's sign.
    Mod,
    /// `//`: floor division (Lua's).
    FloorDiv,
    Pow,
}

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Num(f64),
    T,
    /// Named variable `k` of an expression parsed with
    /// [`Expr::parse_vars`] (V09-S51, the generators' sandbox).
    Var(u8),
    Neg(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
}

/// Why an expression was refused or could not be evaluated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExprError {
    pub message: String,
    /// Character position (0-based) the problem was found at, when known.
    pub at: Option<usize>,
}

impl ExprError {
    fn at(at: usize, message: impl Into<String>) -> Self {
        ExprError {
            message: message.into(),
            at: Some(at),
        }
    }

    fn eval(message: impl Into<String>) -> Self {
        ExprError {
            message: message.into(),
            at: None,
        }
    }
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.at {
            Some(a) => write!(f, "{} (at character {})", self.message, a + 1),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ExprError {}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

/// The token of `//` (floor division).
const FLOOR_DIV: char = '\u{2afd}';

fn tokenize(src: &str) -> Result<Vec<(usize, Tok)>, ExprError> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            // Exponent: e / E followed by an optional sign and digits.
            if i < chars.len()
                && (chars[i] == 'e' || chars[i] == 'E')
                && (chars.get(i + 1).is_some_and(char::is_ascii_digit)
                    || (matches!(chars.get(i + 1), Some('+' | '-'))
                        && chars.get(i + 2).is_some_and(char::is_ascii_digit)))
            {
                i += 2;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text: String = chars[start..i].iter().collect();
            if text.matches('.').count() > 1 {
                return Err(ExprError::at(start, format!("\"{text}\" is not a number")));
            }
            let v: f64 = text
                .parse()
                .map_err(|_| ExprError::at(start, format!("\"{text}\" is not a number")))?;
            if !v.is_finite() {
                return Err(ExprError::at(start, "That number is too large"));
            }
            out.push((start, Tok::Num(v)));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let mut name: String = chars[start..i]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            // Lua's `math.` library names (`math.sin`, `math.pi`): one token.
            if name == "math"
                && chars.get(i) == Some(&'.')
                && chars
                    .get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == '_')
            {
                i += 1;
                let m = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                name.push('.');
                name.extend(chars[m..i].iter().map(char::to_ascii_lowercase));
            }
            out.push((start, Tok::Ident(name)));
            continue;
        }
        // `//`: floor division (one token).
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            out.push((start, Tok::Op(FLOOR_DIV)));
            i += 2;
            continue;
        }
        let tok = match c {
            '+' | '-' | '*' | '/' | '^' | '%' => Tok::Op(c),
            '\u{2212}' => Tok::Op('-'),
            '\u{d7}' => Tok::Op('*'),
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            ',' => Tok::Comma,
            other => {
                return Err(ExprError::at(
                    start,
                    format!(
                        "\"{other}\" is not allowed; use numbers, the variables, pi, e, + - * / % // ^, parentheses and the listed functions"
                    ),
                ));
            }
        };
        out.push((start, tok));
        i += 1;
    }
    Ok(out)
}

struct Parser {
    toks: Vec<(usize, Tok)>,
    pos: usize,
    nodes: usize,
    end: usize,
    /// Named variables (`None`: the curve language's single `t`).
    vars: Option<Vec<String>>,
    /// Named constants ([`Expr::parse_with`]): replaced by their values.
    consts: Vec<(String, f64)>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.1)
    }

    fn here(&self) -> usize {
        self.toks.get(self.pos).map_or(self.end, |t| t.0)
    }

    fn node(&mut self) -> Result<(), ExprError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(ExprError::at(
                self.here(),
                format!("The expression is too complex (more than {MAX_NODES} parts)"),
            ));
        }
        Ok(())
    }

    fn depth(&self, d: usize) -> Result<(), ExprError> {
        if d > MAX_DEPTH {
            return Err(ExprError::at(
                self.here(),
                format!("The expression is nested too deeply (more than {MAX_DEPTH} levels)"),
            ));
        }
        Ok(())
    }

    fn sum(&mut self, d: usize) -> Result<Node, ExprError> {
        self.depth(d)?;
        let mut left = self.product(d)?;
        while let Some(Tok::Op(c @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let right = self.product(d)?;
            self.node()?;
            let op = if c == '+' { Op::Add } else { Op::Sub };
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn product(&mut self, d: usize) -> Result<Node, ExprError> {
        self.depth(d)?;
        let mut left = self.unary(d)?;
        while let Some(Tok::Op(c @ ('*' | '/' | '%' | FLOOR_DIV))) = self.peek().cloned() {
            self.pos += 1;
            let right = self.unary(d)?;
            self.node()?;
            let op = match c {
                '*' => Op::Mul,
                '/' => Op::Div,
                '%' => Op::Mod,
                _ => Op::FloorDiv,
            };
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self, d: usize) -> Result<Node, ExprError> {
        self.depth(d)?;
        match self.peek() {
            Some(Tok::Op('-')) => {
                self.pos += 1;
                let inner = self.unary(d + 1)?;
                self.node()?;
                Ok(Node::Neg(Box::new(inner)))
            }
            Some(Tok::Op('+')) => {
                self.pos += 1;
                self.unary(d + 1)
            }
            _ => self.power(d),
        }
    }

    fn power(&mut self, d: usize) -> Result<Node, ExprError> {
        self.depth(d)?;
        let base = self.primary(d)?;
        if let Some(Tok::Op('^')) = self.peek() {
            self.pos += 1;
            // Right-associative; the exponent may carry its own sign.
            let exp = self.unary(d + 1)?;
            self.node()?;
            return Ok(Node::Bin(Op::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn primary(&mut self, d: usize) -> Result<Node, ExprError> {
        self.depth(d)?;
        let at = self.here();
        let Some(tok) = self.peek().cloned() else {
            return Err(ExprError::at(at, "The expression ends too early"));
        };
        self.pos += 1;
        self.node()?;
        match tok {
            Tok::Num(v) => Ok(Node::Num(v)),
            Tok::LParen => {
                let inner = self.sum(d + 1)?;
                if self.peek() != Some(&Tok::RParen) {
                    return Err(ExprError::at(self.here(), "A \")\" is missing"));
                }
                self.pos += 1;
                Ok(inner)
            }
            Tok::Ident(name) => {
                if let Some(member) = name.strip_prefix("math.") {
                    match member {
                        "huge" => {
                            return Err(ExprError::at(
                                at,
                                "math.huge is infinity, which is not allowed; results must be finite numbers",
                            ));
                        }
                        "random" | "randomseed" => {
                            return Err(ExprError::at(
                                at,
                                format!(
                                    "math.{member} is not available (results must be repeatable); use the seeded variable rnd where the tool offers it"
                                ),
                            ));
                        }
                        "pi" if self.peek() != Some(&Tok::LParen) => {
                            return Ok(Node::Num(std::f64::consts::PI));
                        }
                        _ => {}
                    }
                }
                if self.peek() == Some(&Tok::LParen) {
                    let f = match name.strip_prefix("math.") {
                        Some("log") => Some(Func::MathLog),
                        Some(m) => Func::from_name(m),
                        None => Func::from_name(&name),
                    };
                    let Some(f) = f else {
                        return Err(ExprError::at(
                            at,
                            format!(
                                "\"{name}\" is not an allowed function. Allowed: {} (also as math.<name>)",
                                FUNCTIONS.join(", ")
                            ),
                        ));
                    };
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() == Some(&Tok::RParen) {
                        return Err(ExprError::at(
                            self.here(),
                            format!("{name}() needs a value"),
                        ));
                    }
                    loop {
                        args.push(self.sum(d + 1)?);
                        match self.peek() {
                            Some(Tok::Comma) => self.pos += 1,
                            Some(Tok::RParen) => {
                                self.pos += 1;
                                break;
                            }
                            _ => {
                                return Err(ExprError::at(
                                    self.here(),
                                    format!("A \",\" or \")\" is missing in {name}(…)"),
                                ));
                            }
                        }
                    }
                    let (lo, hi) = f.arity();
                    if args.len() < lo || args.len() > hi {
                        let want = if lo == hi {
                            format!("{lo}")
                        } else {
                            format!("{lo} to {hi}")
                        };
                        return Err(ExprError::at(
                            at,
                            format!("{}() takes {want} value(s), not {}", f.name(), args.len()),
                        ));
                    }
                    Ok(Node::Call(f, args))
                } else if let Some(vars) = &self.vars {
                    match name.as_str() {
                        "pi" => Ok(Node::Num(std::f64::consts::PI)),
                        "e" => Ok(Node::Num(std::f64::consts::E)),
                        _ if Func::from_name(name.trim_start_matches("math.")).is_some()
                            || name == "math.log" =>
                        {
                            Err(ExprError::at(
                                at,
                                format!("{name} is a function; write {name}(…)"),
                            ))
                        }
                        _ => match vars.iter().position(|v| *v == name) {
                            Some(k) => Ok(Node::Var(k as u8)),
                            None => match self.consts.iter().find(|(c, _)| *c == name) {
                                Some(&(_, v)) => Ok(Node::Num(v)),
                                None => Err(ExprError::at(
                                    at,
                                    format!(
                                        "\"{name}\" is not allowed; the variables are {} (constants: pi, e{})",
                                        vars.join(", "),
                                        self.consts
                                            .iter()
                                            .map(|(c, _)| format!(", {c}"))
                                            .collect::<String>()
                                    ),
                                )),
                            },
                        },
                    }
                } else {
                    match name.as_str() {
                        "t" => Ok(Node::T),
                        "pi" => Ok(Node::Num(std::f64::consts::PI)),
                        "e" => Ok(Node::Num(std::f64::consts::E)),
                        _ if Func::from_name(name.trim_start_matches("math.")).is_some()
                            || name == "math.log" =>
                        {
                            Err(ExprError::at(
                                at,
                                format!("{name} is a function; write {name}(…)"),
                            ))
                        }
                        _ => Err(ExprError::at(
                            at,
                            format!(
                                "\"{name}\" is not allowed; the only variable is t (constants: pi, e)"
                            ),
                        )),
                    }
                }
            }
            Tok::Op(c) => Err(ExprError::at(
                at,
                format!("\"{c}\" needs a value before it"),
            )),
            Tok::RParen => Err(ExprError::at(at, "\")\" without a matching \"(\"")),
            Tok::Comma => Err(ExprError::at(
                at,
                "\",\" is only allowed between function values",
            )),
        }
    }
}

/// Most named variables of [`Expr::parse_vars`].
pub const MAX_VARS: usize = 16;

/// A parsed custom curve function.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    source: String,
    root: Node,
    nodes: usize,
    /// The named variables ([`Expr::parse_vars`]); empty for a curve in `t`.
    vars: Vec<String>,
}

impl Expr {
    /// Parses `source` (see module docs). Nothing is evaluated.
    pub fn parse(source: &str) -> Result<Expr, ExprError> {
        Self::parse_in(source, None, Vec::new())
    }

    /// Parses `source` with the named variables `vars` (lower-case
    /// identifiers, at most [`MAX_VARS`], not `pi`, `e` or a function name)
    /// instead of `t` — the generators' math-only sandbox (v0.9 §240): the
    /// same language and the same bounds (length, nodes, nesting); one
    /// evaluation costs at most [`Expr::cost`] steps and allocates nothing.
    pub fn parse_vars(source: &str, vars: &[&str]) -> Result<Expr, ExprError> {
        Self::parse_with(source, vars, &[])
    }

    /// [`Expr::parse_vars`] with named constants `consts` (each name a valid
    /// variable name that is not one of `vars`): a constant is replaced by
    /// its value while parsing (B1-0075, Note Properties' user constants).
    pub fn parse_with(
        source: &str,
        vars: &[&str],
        consts: &[(&str, f64)],
    ) -> Result<Expr, ExprError> {
        assert!(vars.len() <= MAX_VARS, "at most {MAX_VARS} variables");
        for v in vars.iter().chain(consts.iter().map(|c| &c.0)) {
            assert!(
                valid_name(v) && !consts.iter().any(|c| vars.contains(&c.0)),
                "bad variable or constant name {v:?}"
            );
        }
        Self::parse_in(
            source,
            Some(vars.iter().map(|v| (*v).to_owned()).collect()),
            consts.iter().map(|(n, v)| ((*n).to_owned(), *v)).collect(),
        )
    }

    fn parse_in(
        source: &str,
        vars: Option<Vec<String>>,
        consts: Vec<(String, f64)>,
    ) -> Result<Expr, ExprError> {
        let trimmed = source.trim();
        if trimmed.is_empty() {
            return Err(ExprError::at(
                0,
                match &vars {
                    None => "Enter a formula in t, for example t*t".to_owned(),
                    Some(v) => format!("Enter a formula in {}", v.join(", ")),
                },
            ));
        }
        if trimmed.chars().count() > MAX_EXPR_LEN {
            return Err(ExprError::at(
                MAX_EXPR_LEN,
                format!("The expression is longer than {MAX_EXPR_LEN} characters"),
            ));
        }
        let toks = tokenize(trimmed)?;
        let mut p = Parser {
            toks,
            pos: 0,
            nodes: 0,
            end: trimmed.chars().count(),
            vars,
            consts,
        };
        let root = p.sum(0)?;
        if p.pos < p.toks.len() {
            let at = p.here();
            return Err(ExprError::at(
                at,
                match p.peek() {
                    Some(Tok::RParen) => "\")\" without a matching \"(\"".to_owned(),
                    _ => "Something is missing here (an operator such as * between values?)"
                        .to_owned(),
                },
            ));
        }
        Ok(Expr {
            source: trimmed.to_owned(),
            nodes: p.nodes,
            root,
            vars: p.vars.unwrap_or_default(),
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Tree nodes (the cost of one evaluation).
    pub fn cost(&self) -> usize {
        self.nodes
    }

    /// The value at `t`. Errors (with the reason) for results that are not
    /// finite numbers or for arguments outside a function's domain.
    pub fn eval(&self, t: f64) -> Result<f64, ExprError> {
        self.eval_vars(&[t])
    }

    /// The value with the named variables (in [`Expr::parse_vars`] order)
    /// set to `values` (missing ones are 0). Errors as [`Expr::eval`].
    pub fn eval_vars(&self, values: &[f64]) -> Result<f64, ExprError> {
        let mut vals = [0f64; MAX_VARS];
        for (d, s) in vals.iter_mut().zip(values) {
            *d = *s;
        }
        let at = Where {
            names: &self.vars,
            vals: &vals,
        };
        let v = eval(&self.root, &vals, &at)?;
        if v.is_finite() {
            Ok(v)
        } else {
            Err(ExprError::eval(format!(
                "The formula gives no finite value {}",
                at.text()
            )))
        }
    }

    /// The named variables (empty for a curve in `t`).
    pub fn vars(&self) -> &[String] {
        &self.vars
    }

    /// Whether the expression reads the named variable `name` (B1-0075:
    /// per-note values are computed only when a field reads them).
    pub fn uses(&self, name: &str) -> bool {
        let Some(k) = self.vars.iter().position(|v| v == name) else {
            return self.vars.is_empty() && name == "t" && reads(&self.root, None);
        };
        reads(&self.root, Some(k as u8))
    }
}

/// A variable or constant name: lower-case ASCII letters, digits and `_`,
/// starting with a letter or `_`, not `pi`, `e`, `math` or a function.
pub fn valid_name(v: &str) -> bool {
    v.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && v.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !["pi", "e", "math"].contains(&v)
        && Func::from_name(v).is_none()
}

/// Whether `n` reads variable `k` (`None`: the curve language's `t`).
fn reads(n: &Node, k: Option<u8>) -> bool {
    match n {
        Node::Num(_) => false,
        Node::T => k.is_none(),
        Node::Var(j) => k == Some(*j),
        Node::Neg(a) => reads(a, k),
        Node::Bin(_, a, b) => reads(a, k) || reads(b, k),
        Node::Call(_, args) => args.iter().any(|a| reads(a, k)),
    }
}

/// Where an evaluation failed (built only for its message).
struct Where<'a> {
    names: &'a [String],
    vals: &'a [f64; MAX_VARS],
}

impl Where<'_> {
    fn text(&self) -> String {
        if self.names.is_empty() {
            return format!("at t = {}", fmt_t(self.vals[0]));
        }
        let parts: Vec<String> = self
            .names
            .iter()
            .zip(self.vals.iter())
            .map(|(n, v)| format!("{n} = {}", fmt_t(*v)))
            .collect();
        format!("at {}", parts.join(", "))
    }
}

fn fmt_t(t: f64) -> String {
    let s = format!("{t:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".into()
    } else {
        s.to_owned()
    }
}

fn eval(n: &Node, vals: &[f64; MAX_VARS], w: &Where) -> Result<f64, ExprError> {
    let at = |what: &str| ExprError::eval(format!("{what} {}", w.text()));
    Ok(match n {
        Node::Num(v) => *v,
        Node::T => vals[0],
        Node::Var(k) => vals[usize::from(*k)],
        Node::Neg(a) => -eval(a, vals, w)?,
        Node::Bin(op, a, b) => {
            let (x, y) = (eval(a, vals, w)?, eval(b, vals, w)?);
            match op {
                Op::Add => x + y,
                Op::Sub => x - y,
                Op::Mul => x * y,
                Op::Div => {
                    if y == 0.0 {
                        return Err(at("Division by zero"));
                    }
                    x / y
                }
                Op::Mod => {
                    if y == 0.0 {
                        return Err(at("Modulo by zero"));
                    }
                    floored_mod(x, y)
                }
                Op::FloorDiv => {
                    if y == 0.0 {
                        return Err(at("Division by zero"));
                    }
                    (x / y).floor()
                }
                Op::Pow => pow(x, y).ok_or_else(|| at("A power has no real value"))?,
            }
        }
        Node::Call(f, args) => {
            let mut v = [0f64; 16];
            for (k, a) in args.iter().enumerate() {
                v[k] = eval(a, vals, w)?;
            }
            let x = v[0];
            match f {
                Func::Abs => x.abs(),
                Func::Min => v[..args.len()]
                    .iter()
                    .copied()
                    .fold(f64::INFINITY, f64::min),
                Func::Max => v[..args.len()]
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max),
                Func::Sqrt => {
                    if x < 0.0 {
                        return Err(at("sqrt() of a negative number"));
                    }
                    x.sqrt()
                }
                Func::Sin => x.sin(),
                Func::Cos => x.cos(),
                Func::Tan => x.tan(),
                Func::Exp => x.exp(),
                Func::Ln | Func::Log => {
                    if x <= 0.0 {
                        return Err(at(&format!("{}() of zero or a negative number", f.name())));
                    }
                    if *f == Func::Ln { x.ln() } else { x.log10() }
                }
                Func::Pow => pow(x, v[1]).ok_or_else(|| at("pow() has no real value"))?,
                Func::Clamp => {
                    let (lo, hi) = (v[1], v[2]);
                    if lo > hi {
                        return Err(at("clamp() needs its lower bound below its upper bound"));
                    }
                    x.clamp(lo, hi)
                }
                Func::Floor => x.floor(),
                Func::Ceil => x.ceil(),
                Func::Round => x.round(),
                Func::Trunc => x.trunc(),
                Func::Sign => {
                    if x == 0.0 {
                        0.0
                    } else {
                        x.signum()
                    }
                }
                Func::Frac => x - x.floor(),
                Func::Mod | Func::Fmod => {
                    let y = v[1];
                    if y == 0.0 {
                        return Err(at(&format!("{}() by zero", f.name())));
                    }
                    if *f == Func::Mod {
                        floored_mod(x, y)
                    } else {
                        x % y
                    }
                }
                Func::Asin | Func::Acos => {
                    if !(-1.0..=1.0).contains(&x) {
                        return Err(at(&format!("{}() of a number outside -1 … 1", f.name())));
                    }
                    if *f == Func::Asin { x.asin() } else { x.acos() }
                }
                Func::Atan => {
                    if args.len() == 2 {
                        x.atan2(v[1])
                    } else {
                        x.atan()
                    }
                }
                Func::Atan2 => x.atan2(v[1]),
                Func::Hypot => x.hypot(v[1]),
                Func::MathLog => {
                    if x <= 0.0 {
                        return Err(at("math.log() of zero or a negative number"));
                    }
                    if args.len() == 2 {
                        let b = v[1];
                        if b <= 0.0 || b == 1.0 {
                            return Err(at("math.log() needs a base above 0 other than 1"));
                        }
                        x.ln() / b.ln()
                    } else {
                        x.ln()
                    }
                }
            }
        }
    })
}

/// Lua's `%`: `x - floor(x / y) * y`, the divisor's sign; never outside
/// `[0, |y|)` (in the divisor's direction) through rounding.
fn floored_mod(x: f64, y: f64) -> f64 {
    let r = x % y;
    if r != 0.0 && (r < 0.0) != (y < 0.0) {
        let s = r + y;
        // `r + y` can round to `y` itself for a tiny `r`.
        if s == y { 0.0 } else { s }
    } else {
        r
    }
}

/// `x^y` with a real result (a negative base only for whole exponents).
fn pow(x: f64, y: f64) -> Option<f64> {
    if x < 0.0 && y.fract() != 0.0 {
        return None;
    }
    if x == 0.0 && y < 0.0 {
        return None;
    }
    Some(x.powf(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V09-S51 (§240): named variables for the generators, the same
    /// language and bounds; unknown names are refused with the list.
    #[test]
    fn named_variables_evaluate_within_the_same_bounds() {
        let e = Expr::parse_vars("x*cos(a) - y*sin(a) + n", &["x", "y", "a", "n"]).unwrap();
        let got = e
            .eval_vars(&[2.0, 1.0, std::f64::consts::FRAC_PI_2, 10.0])
            .unwrap();
        assert!((got - 9.0).abs() < 1e-12);
        assert_eq!(e.vars().len(), 4);
        let err = Expr::parse_vars("x + t", &["x", "y"]).unwrap_err();
        assert!(err.message.contains("the variables are x, y"), "{err}");
        let err = Expr::parse_vars("sqrt(x)", &["x"])
            .unwrap()
            .eval_vars(&[-1.0])
            .unwrap_err();
        assert!(err.message.contains("at x = -1"), "{err}");
        assert!(Expr::parse_vars(&"x+".repeat(600), &["x"]).is_err());
        assert!(Expr::parse_vars(&vec!["x"; 300].join("+"), &["x"]).is_err());
        // The curve language is unchanged: only t.
        assert!(Expr::parse("x").is_err());
        assert_eq!(Expr::parse("t*2").unwrap().eval(0.25).unwrap(), 0.5);
        assert_eq!(Expr::parse("t").unwrap().vars().len(), 0);
    }

    /// B1-0075 / B1-0076: `%` (floored, binding like `*`), `//`, the new
    /// functions and Lua's `math.` names; every older formula keeps its value.
    #[test]
    fn modulo_floor_division_and_lua_math_names_evaluate() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
        assert_eq!(v("7 % 3", 0.0), 1.0);
        assert_eq!(v("-1 % 16", 0.0), 15.0, "floored: the divisor's sign");
        assert_eq!(v("7 % -3", 0.0), -2.0);
        assert_eq!(
            v("2 + 7 % 3 * 2", 0.0),
            4.0,
            "% binds like * and /, left to right"
        );
        assert_eq!(v("t % 1", -0.25), 0.75);
        assert!(v("t % 1", -1e-20) < 1.0, "never the divisor itself");
        assert_eq!(v("7 // 2", 0.0), 3.0);
        assert_eq!(v("-7 // 2", 0.0), -4.0);
        assert_eq!(v("mod(-1, 16)", 0.0), 15.0);
        assert_eq!(v("fmod(-1, 16)", 0.0), -1.0, "fmod keeps the sign of x");
        assert_eq!(v("math.fmod(7, 3)", 0.0), 1.0);
        assert_eq!(
            v("round(2.5) + round(-2.5)", 0.0),
            0.0,
            "half away from zero"
        );
        assert_eq!(v("trunc(-2.7)", 0.0), -2.0);
        assert_eq!(v("sign(-3) + sign(0) + sign(5)", 0.0), 0.0);
        assert_eq!(v("frac(-1.25)", 0.0), 0.75);
        assert!(close(v("asin(1)", 0.0), std::f64::consts::FRAC_PI_2));
        assert!(close(v("acos(1)", 0.0), 0.0));
        assert!(close(v("atan(1)", 0.0), std::f64::consts::FRAC_PI_4));
        assert!(close(
            v("atan(1, -1)", 0.0),
            3.0 * std::f64::consts::FRAC_PI_4
        ));
        assert!(close(
            v("atan2(1, -1)", 0.0),
            3.0 * std::f64::consts::FRAC_PI_4
        ));
        assert_eq!(v("hypot(3, 4)", 0.0), 5.0);
        assert!(close(v("math.sin(math.pi / 2)", 0.0), 1.0));
        assert!(
            close(v("MATH.LOG(e)", 0.0), 1.0),
            "math.log is natural, as in Lua"
        );
        assert!(close(v("math.log(8, 2)", 0.0), 3.0));
        assert_eq!(v("log(1000)", 0.0), 3.0, "log stays base 10");
        assert_eq!(
            v("math.floor(2.7) + math.abs(-1) + math.max(1, 4)", 0.0),
            7.0
        );
        // The owner's image16 channel formula (a GLSL-style hash: `% 1` is
        // the fraction) evaluates to a finite value in [0, 1).
        let h = Expr::parse_vars(
            "((math.sin(t*12.9898 + k*78.233) * 43758.5453) % 1)",
            &["t", "k"],
        )
        .unwrap();
        for (t, k) in [(0.0, 60.0), (480.0, 61.0), (1e9, 127.0), (123_456.0, 0.0)] {
            let x = h.eval_vars(&[t, k]).unwrap();
            assert!((0.0..1.0).contains(&x), "{t} {k}: {x}");
        }
    }

    /// B1-0076: refused inputs and domain errors (never a silent value).
    #[test]
    fn new_operators_refuse_bad_values() {
        for bad in [
            "math.huge",
            "math.random()",
            "math.randomseed(1)",
            "math.foo(t)",
            "t %",
            "% t",
            "t /// 2",
            "math.",
            "mod(t)",
            "hypot(t)",
            "atan(t, 1, 2)",
        ] {
            assert!(Expr::parse(bad).is_err(), "{bad} must be refused");
        }
        let e = Expr::parse("math.huge").unwrap_err();
        assert!(e.message.contains("infinity"), "{e}");
        let e = Expr::parse("math.random()").unwrap_err();
        assert!(e.message.contains("rnd"), "{e}");
        for (src, t) in [
            ("t % 0", 1.0),
            ("t // 0", 1.0),
            ("mod(t, 0)", 1.0),
            ("fmod(t, 0)", 1.0),
            ("asin(t)", 2.0),
            ("acos(t)", -1.5),
            ("math.log(t)", 0.0),
            ("math.log(t, 1)", 4.0),
        ] {
            assert!(Expr::parse(src).unwrap().eval(t).is_err(), "{src} at {t}");
        }
        assert!(
            Expr::parse("t % 0")
                .unwrap()
                .eval(1.0)
                .unwrap_err()
                .message
                .contains("Modulo by zero")
        );
    }

    /// B1-0075: named constants are values, `uses` reports what is read,
    /// and the variable / constant names are checked.
    #[test]
    fn constants_and_used_variables() {
        let e = Expr::parse_with("a * x + b", &["x", "y"], &[("a", 2.0), ("b", 0.5)]).unwrap();
        assert_eq!(e.eval_vars(&[3.0, 9.0]).unwrap(), 6.5);
        assert!(e.uses("x") && !e.uses("y") && !e.uses("a"));
        assert!(Expr::parse("t*2").unwrap().uses("t"));
        assert!(!Expr::parse("2").unwrap().uses("t"));
        let err = Expr::parse_with("q", &["x"], &[("a", 1.0)]).unwrap_err();
        assert!(err.message.contains("constants: pi, e, a"), "{err}");
        assert!(valid_name("sc") && valid_name("k2") && valid_name("_x"));
        for bad in ["", "2k", "pi", "e", "math", "sin", "frac", "K", "a-b"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    fn v(src: &str, t: f64) -> f64 {
        Expr::parse(src).unwrap().eval(t).unwrap()
    }

    #[test]
    fn documented_examples_evaluate() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
        for t in [0.0, 0.25, 0.5, 1.0] {
            assert!(close(v("t", t), t));
            assert!(close(v("t*t", t), t * t));
            assert!(close(v("sqrt(t)", t), t.sqrt()));
            assert!(close(
                v("sin(pi*t/2)", t),
                (std::f64::consts::PI * t / 2.0).sin()
            ));
            assert!(close(
                v("(1-cos(pi*t))/2", t),
                (1.0 - (std::f64::consts::PI * t).cos()) / 2.0
            ));
            assert!(close(v("t*t*(3-2*t)", t), t * t * (3.0 - 2.0 * t)));
        }
        assert_eq!(v("2^3^2", 0.0), 512.0); // right-associative
        assert_eq!(v("-2^2", 0.0), -4.0);
        assert_eq!(v("2^-1", 0.0), 0.5);
        assert_eq!(v("min(3, 1, 2)", 0.0), 1.0);
        assert_eq!(v("max(t, 0.5)", 0.25), 0.5);
        assert_eq!(v("clamp(t*2, 0, 1)", 0.75), 1.0);
        assert_eq!(v("pow(2, 10)", 0.0), 1024.0);
        assert_eq!(v("log(1000)", 0.0), 3.0);
        assert!((v("ln(e)", 0.0) - 1.0).abs() < 1e-15);
        assert_eq!(v("floor(2.7) + ceil(2.1)", 0.0), 5.0);
        assert_eq!(v("abs(-t)", 0.5), 0.5);
        assert_eq!(v("1.5e-1 * 2", 0.0), 0.3);
        assert_eq!(v("  T * 2 ", 0.5), 1.0, "names are case-insensitive");
        assert_eq!(v("2 × t − 1", 1.0), 1.0, "typographic × and − work");
    }

    #[test]
    fn code_injection_and_unknown_names_are_refused() {
        for bad in [
            "system(\"rm -rf /\")",
            "std::process::Command",
            "import os",
            "t; drop",
            "exec(t)",
            "x",
            "t = 1",
            "open('/etc/passwd')",
            "require(\"fs\")",
            "$t",
            "t`ls`",
            "__proto__",
            "sin",
            "eval(t)",
            "0x10",
            "1..2",
            "",
            "(t",
            "t)",
            "t t",
            "sin()",
            "pow(t)",
            "clamp(t, 1)",
            "*t",
            "t,1",
        ] {
            assert!(Expr::parse(bad).is_err(), "{bad} must be refused");
        }
        let e = Expr::parse("system(1)").unwrap_err();
        assert!(e.message.contains("not an allowed function"), "{e}");
    }

    #[test]
    fn resource_limits_hold() {
        let deep = format!("{}t{}", "(".repeat(200), ")".repeat(200));
        let e = Expr::parse(&deep).unwrap_err();
        assert!(e.message.contains("nested too deeply"), "{e}");
        let long = "t+".repeat(600) + "t";
        assert!(
            Expr::parse(&long)
                .unwrap_err()
                .message
                .contains("longer than")
        );
        let many = vec!["t"; 300].join("+");
        assert!(
            Expr::parse(&many)
                .unwrap_err()
                .message
                .contains("too complex")
        );
        let neg = "-".repeat(500) + "t";
        assert!(Expr::parse(&neg).is_err());
        // Right at the limits it still works.
        let ok = vec!["t"; 200].join("+");
        assert_eq!(Expr::parse(&ok).unwrap().eval(1.0).unwrap(), 200.0);
    }

    #[test]
    fn invalid_domains_are_errors_not_values() {
        for (src, t) in [
            ("sqrt(t-1)", 0.5),
            ("1/t", 0.0),
            ("ln(t)", 0.0),
            ("log(-t)", 0.5),
            ("(-t)^0.5", 0.5),
            ("0^-1", 0.0),
            ("exp(1000)", 0.0),
            ("clamp(t, 1, 0)", 0.5),
            ("10^400", 0.0),
        ] {
            assert!(Expr::parse(src).unwrap().eval(t).is_err(), "{src} at {t}");
        }
        // Whole powers of negative numbers are fine.
        assert_eq!(v("(-2)^3", 0.0), -8.0);
        let e = Expr::parse("1/(t-0.5)").unwrap().eval(0.5).unwrap_err();
        assert!(e.message.contains("t = 0.5"), "{e}");
    }
}
