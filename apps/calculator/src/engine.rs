//! Expression engine for the calculator.
//!
//! A compact, dependency-free math evaluator: tokenizer → Pratt parser →
//! AST → tree-walking evaluator. We own this rather than pulling a crate so
//! the build stays green offline AND so we get exact control over the two
//! features a graphing calculator actually needs:
//!
//!   * a **degrees/radians** toggle that affects `sin/cos/tan` and their
//!     inverses (the popular crates hard-wire trig to radians), and
//!   * a **compiled** form that binds `x` (and `x,y`) so the 2D/3D plotters
//!     can sample a parsed expression thousands of times without re-parsing.
//!
//! Grammar (informally):
//!   expr   := term (('+' | '-') term)*
//!   term   := power (('*' | '/') power | implicit-mult)*
//!   power  := unary ('^' power)?        // right associative
//!   unary  := ('-' | '+')* postfix
//!   postfix:= atom ('!' | '%')*
//!   atom   := number | const | var | name '(' args ')' | '(' expr ')'
//!
//! Implicit multiplication binds at `*` precedence: `2pi`, `3(x+1)`, `2x`.

use std::f64::consts::{E, PI, TAU};
use std::iter::Peekable;
use std::str::Chars;

/// Parsed expression node.
#[derive(Clone, Debug)]
pub enum Expr {
    Num(f64),
    Var(String),
    Neg(Box<Expr>),
    Fact(Box<Expr>),
    Percent(Box<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

/// Evaluation context: angle mode + bound variables.
#[derive(Clone, Copy, Default)]
pub struct Ctx {
    pub deg: bool,
    pub x: f64,
    pub y: f64,
    pub ans: f64,
    pub mem: f64,
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

fn tokenize(input: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        match c {
            '0'..='9' | '.' => out.push(Tok::Num(scan_number(c, &mut chars)?)),
            'a'..='z' | 'A'..='Z' | '_' => out.push(Tok::Ident(scan_ident(c, &mut chars))),
            // Unicode niceties accepted in case they leak in from a paste.
            'π' => out.push(Tok::Ident("pi".into())),
            '×' => out.push(Tok::Op('*')),
            '÷' => out.push(Tok::Op('/')),
            '−' => out.push(Tok::Op('-')),
            '√' => out.push(Tok::Ident("sqrt".into())),
            '+' | '-' | '*' | '/' | '^' | '!' | '%' => out.push(Tok::Op(c)),
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            ',' => out.push(Tok::Comma),
            _ => return Err(format!("unexpected `{c}`")),
        }
    }
    Ok(out)
}

/// Scan the rest of a number whose first character `first` is already
/// consumed: digits and dots, then an optional scientific exponent.
///
/// The exponent needs two characters of lookahead (`e`, an optional sign, then
/// at least one digit), so it is scanned on a cloned iterator and only
/// committed once a digit confirms it — otherwise the `e` starts the next
/// token, which is what makes `2e` parse as `2 * e`.
fn scan_number(first: char, chars: &mut Peekable<Chars<'_>>) -> Result<f64, String> {
    let mut s = String::from(first);
    take_while_into(&mut s, chars, |c| c.is_ascii_digit() || c == '.');
    if matches!(chars.peek(), Some('e' | 'E')) {
        let mut probe = chars.clone();
        probe.next();
        let sign = match probe.peek() {
            Some(&c @ ('+' | '-')) => {
                probe.next();
                Some(c)
            }
            _ => None,
        };
        if probe.peek().is_some_and(char::is_ascii_digit) {
            s.push('e');
            if let Some(c) = sign {
                s.push(c);
            }
            *chars = probe;
            take_while_into(&mut s, chars, |c| c.is_ascii_digit());
        }
    }
    s.parse().map_err(|_| format!("bad number `{s}`"))
}

/// Scan the rest of an identifier whose first character is already consumed.
fn scan_ident(first: char, chars: &mut Peekable<Chars<'_>>) -> String {
    let mut s = String::from(first);
    take_while_into(&mut s, chars, |c| c.is_ascii_alphanumeric() || c == '_');
    s.to_ascii_lowercase()
}

/// Append every leading character matching `keep` to `out`, consuming them.
fn take_while_into(out: &mut String, chars: &mut Peekable<Chars<'_>>, keep: impl Fn(char) -> bool) {
    while let Some(&c) = chars.peek() {
        if !keep(c) {
            break;
        }
        out.push(c);
        chars.next();
    }
}

// ---------------------------------------------------------------------------
// Parser (precedence climbing)
// ---------------------------------------------------------------------------

/// The token stream still to be parsed. Consuming from the front of a slice
/// (rather than advancing an index into a `Vec`) keeps "how much is left" and
/// "what comes next" the same question, so neither can run past the end.
struct Parser<'t> {
    toks: &'t [Tok],
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.first()
    }
    fn next(&mut self) -> Option<Tok> {
        let (first, rest) = self.toks.split_first()?;
        self.toks = rest;
        Some(first.clone())
    }

    fn parse_bp(&mut self, min_bp: u8) -> Result<Expr, String> {
        let mut lhs = self.parse_prefix()?;
        while let Some(tok) = self.peek().cloned() {
            match tok {
                Tok::Op('!') if 50 >= min_bp => {
                    self.next();
                    lhs = Expr::Fact(Box::new(lhs));
                }
                Tok::Op('%') if 50 >= min_bp => {
                    self.next();
                    lhs = Expr::Percent(Box::new(lhs));
                }
                Tok::Op('^') if 40 >= min_bp => {
                    self.next();
                    let rhs = self.parse_bp(39)?; // right-assoc
                    lhs = Expr::Bin('^', Box::new(lhs), Box::new(rhs));
                }
                Tok::Op(op @ ('*' | '/')) if 20 >= min_bp => {
                    self.next();
                    let rhs = self.parse_bp(21)?;
                    lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
                }
                Tok::Op(op @ ('+' | '-')) if 10 >= min_bp => {
                    self.next();
                    let rhs = self.parse_bp(11)?;
                    lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
                }
                // Implicit multiplication: a value-start token directly
                // following an operand (`2pi`, `3(x+1)`, `2sin(x)`).
                Tok::Num(_) | Tok::Ident(_) | Tok::LParen if 20 >= min_bp => {
                    let rhs = self.parse_bp(21)?;
                    lhs = Expr::Bin('*', Box::new(lhs), Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<Expr, String> {
        let tok = self.next().ok_or_else(|| "unexpected end".to_string())?;
        match tok {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Op('-') => Ok(Expr::Neg(Box::new(self.parse_bp(30)?))),
            Tok::Op('+') => self.parse_bp(30),
            Tok::LParen => {
                let e = self.parse_bp(0)?;
                match self.next() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err("expected `)`".to_string()),
                }
            }
            Tok::Ident(name) => {
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.next(); // consume '('
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some(Tok::RParen)) {
                        loop {
                            args.push(self.parse_bp(0)?);
                            match self.peek() {
                                Some(Tok::Comma) => {
                                    self.next();
                                }
                                _ => break,
                            }
                        }
                    }
                    match self.next() {
                        Some(Tok::RParen) => Ok(Expr::Call(name, args)),
                        _ => Err("expected `)`".to_string()),
                    }
                } else {
                    Ok(Expr::Var(name))
                }
            }
            other => Err(format!("unexpected token {other:?}")),
        }
    }
}

/// Parse `input` into an AST.
///
/// # Errors
/// A human-readable message (which the UI shows in the result line) when the
/// input is empty, holds a character the tokenizer does not accept, or is not
/// a complete expression — an unbalanced `(`, a dangling operator, or trailing
/// tokens after one.
pub fn parse(input: &str) -> Result<Expr, String> {
    let toks = tokenize(input)?;
    if toks.is_empty() {
        return Err("empty".to_string());
    }
    let mut p = Parser { toks: &toks };
    let e = p.parse_bp(0)?;
    if !p.toks.is_empty() {
        return Err("trailing input".to_string());
    }
    Ok(e)
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

fn eval_node(expr: &Expr, ctx: &Ctx) -> Result<f64, String> {
    Ok(match expr {
        Expr::Num(n) => *n,
        Expr::Var(name) => match name.as_str() {
            "pi" => PI,
            "tau" => TAU,
            "e" => E,
            "x" => ctx.x,
            "y" => ctx.y,
            "ans" => ctx.ans,
            "m" | "mem" => ctx.mem,
            "inf" => f64::INFINITY,
            "nan" => f64::NAN,
            _ => return Err(format!("unknown name `{name}`")),
        },
        Expr::Neg(inner) => -eval_node(inner, ctx)?,
        Expr::Fact(inner) => factorial(eval_node(inner, ctx)?),
        Expr::Percent(inner) => eval_node(inner, ctx)? / 100.0,
        Expr::Bin(op, left, right) => {
            let lhs = eval_node(left, ctx)?;
            let rhs = eval_node(right, ctx)?;
            match op {
                '+' => lhs + rhs,
                '-' => lhs - rhs,
                '*' => lhs * rhs,
                '/' => lhs / rhs,
                '^' => lhs.powf(rhs),
                _ => return Err(format!("bad operator `{op}`")),
            }
        }
        Expr::Call(name, args) => eval_call(name, args, ctx)?,
    })
}

fn eval_call(name: &str, args: &[Expr], ctx: &Ctx) -> Result<f64, String> {
    let mut vals = Vec::with_capacity(args.len());
    for arg in args {
        vals.push(eval_node(arg, ctx)?);
    }
    // Arity is checked by matching the argument slice, so every argument that
    // reaches a function is one the slice pattern proved is there.
    let only = || -> Result<f64, String> {
        match vals.as_slice() {
            [v] => Ok(*v),
            _ => Err(format!("{name}() takes 1 argument")),
        }
    };
    let one = |func: fn(f64) -> f64| only().map(func);
    let pair = |sig: &str| -> Result<(f64, f64), String> {
        match vals.as_slice() {
            [a, b] => Ok((*a, *b)),
            _ => Err(format!("{sig} takes 2 arguments")),
        }
    };
    // Degree-aware trig: argument is degrees when `ctx.deg`.
    let to_rad = if ctx.deg { PI / 180.0 } else { 1.0 };
    let from_rad = if ctx.deg { 180.0 / PI } else { 1.0 };
    Ok(match name {
        "sin" => return only().map(|v| (v * to_rad).sin()),
        "cos" => return only().map(|v| (v * to_rad).cos()),
        "tan" => return only().map(|v| (v * to_rad).tan()),
        "asin" => return only().map(|v| v.asin() * from_rad),
        "acos" => return only().map(|v| v.acos() * from_rad),
        "atan" => return only().map(|v| v.atan() * from_rad),
        "sinh" => return one(f64::sinh),
        "cosh" => return one(f64::cosh),
        "tanh" => return one(f64::tanh),
        "asinh" => return one(f64::asinh),
        "acosh" => return one(f64::acosh),
        "atanh" => return one(f64::atanh),
        "exp" => return one(f64::exp),
        "ln" => return one(f64::ln),
        "log10" => return one(f64::log10),
        "log2" => return one(f64::log2),
        "sqrt" => return one(f64::sqrt),
        "cbrt" => return one(f64::cbrt),
        "abs" => return one(f64::abs),
        "sign" | "sgn" => return one(f64::signum),
        "floor" => return one(f64::floor),
        "ceil" => return one(f64::ceil),
        "round" => return one(f64::round),
        "trunc" => return one(f64::trunc),
        "fract" => return one(f64::fract),
        "rad" => return one(f64::to_radians),
        "deg" => return one(f64::to_degrees),
        "gamma" => return one(gamma),
        "fact" => return one(factorial),
        "log" => match vals.as_slice() {
            [v] => v.log10(),
            [base, v] => v.log(*base), // log(base, value)
            _ => return Err("log() takes 1 or 2 arguments".into()),
        },
        "root" => {
            let (degree, radicand) = pair("root(n, x)")?;
            radicand.powf(1.0 / degree)
        }
        "pow" => {
            let (base, exp) = pair("pow(a, b)")?;
            base.powf(exp)
        }
        "mod" => {
            let (lhs, rhs) = pair("mod(a, b)")?;
            lhs.rem_euclid(rhs)
        }
        "atan2" => {
            let (y, x) = pair("atan2(y, x)")?;
            y.atan2(x) * from_rad
        }
        "hypot" => {
            let (a, b) = pair("hypot(a, b)")?;
            a.hypot(b)
        }
        "max" => {
            if vals.is_empty() {
                return Err("max() needs arguments".into());
            }
            vals.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        }
        "min" => {
            if vals.is_empty() {
                return Err("min() needs arguments".into());
            }
            vals.iter().copied().fold(f64::INFINITY, f64::min)
        }
        _ => return Err(format!("unknown function `{name}`")),
    })
}

/// Parse and evaluate `input` in one shot. Used by the `=` key.
///
/// # Errors
/// The parse error for malformed input, or an evaluation error: an unknown
/// name or function, or a call with the wrong number of arguments. A domain
/// problem is a value, not an error — `1/0` evaluates to infinity.
pub fn evaluate(input: &str, ctx: &Ctx) -> Result<f64, String> {
    let e = parse(input)?;
    eval_node(&e, ctx)
}

/// A parsed expression bound for fast repeated sampling by the plotters.
#[derive(Clone)]
pub struct Compiled {
    ast: Expr,
    deg: bool,
}

impl Compiled {
    /// Sample with a single free variable `x`.
    pub fn eval1(&self, x: f64) -> f64 {
        let ctx = Ctx {
            deg: self.deg,
            x,
            ..Default::default()
        };
        eval_node(&self.ast, &ctx).unwrap_or(f64::NAN)
    }
    /// Sample with two free variables `x`, `y`.
    pub fn eval2(&self, x: f64, y: f64) -> f64 {
        let ctx = Ctx {
            deg: self.deg,
            x,
            y,
            ..Default::default()
        };
        eval_node(&self.ast, &ctx).unwrap_or(f64::NAN)
    }
}

/// Parse + structurally validate an expression for plotting. The sample
/// eval at a representative point surfaces unknown-name / arity errors as
/// `Err`; domain issues (NaN/inf) are values, not errors, so a valid
/// `1/x` compiles fine.
///
/// # Errors
/// The parse error for malformed input, a variable the plot cannot supply
/// (`y` in a 2D plot, or any typo), or an unknown function / wrong arity.
pub fn compile(input: &str, deg: bool, two_var: bool) -> Result<Compiled, String> {
    let ast = parse(input)?;
    let probe = Ctx {
        deg,
        x: 0.7,
        y: 0.3,
        ..Default::default()
    };
    // Walk once to catch unknown names regardless of NaN domain points.
    validate(&ast, two_var)?;
    let _ = eval_node(&ast, &probe)?;
    Ok(Compiled { ast, deg })
}

/// Reject references to variables a plot can't supply (so a typo like
/// `z` in a 2D plot is reported instead of silently erroring per-sample).
fn validate(e: &Expr, two_var: bool) -> Result<(), String> {
    match e {
        Expr::Num(_) => Ok(()),
        Expr::Var(name) => match name.as_str() {
            "pi" | "tau" | "e" | "ans" | "m" | "mem" | "inf" | "nan" | "x" => Ok(()),
            "y" if two_var => Ok(()),
            other => Err(format!("unknown name `{other}`")),
        },
        Expr::Neg(a) | Expr::Fact(a) | Expr::Percent(a) => validate(a, two_var),
        Expr::Bin(_, a, b) => {
            validate(a, two_var)?;
            validate(b, two_var)
        }
        Expr::Call(_, args) => {
            for a in args {
                validate(a, two_var)?;
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Special functions + formatting
// ---------------------------------------------------------------------------

/// Lanczos approximation of the gamma function (g=7, n=9).
pub fn gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection formula for the negative half-line.
        PI / ((PI * x).sin() * gamma(1.0 - x))
    } else {
        let x = x - 1.0;
        let t = x + G + 0.5;
        // The series is `C[0] + sum(C[i] / (x + i))` for i = 1.., so split the
        // head off the table (an irrefutable destructure — no index) and pair
        // the tail with its term number.
        let [head, tail @ ..] = C;
        let mut a = head;
        for (i, coef) in (1u32..).zip(tail) {
            a += coef / (x + f64::from(i));
        }
        (TAU).sqrt() * t.powf(x + 0.5) * (-t).exp() * a
    }
}

/// Factorial via gamma, exact for small non-negative integers.
pub fn factorial(n: f64) -> f64 {
    if n.fract() == 0.0 && n < 0.0 {
        return f64::NAN; // poles at non-positive integers
    }
    if n.fract() == 0.0 && (0.0..=170.0).contains(&n) {
        // Count in integers: a float counter accumulates error, and 170! is the
        // last factorial that fits an f64 (171! overflows to infinity).
        let mut r = 1.0f64;
        for i in 2..=170u32 {
            let factor = f64::from(i);
            if factor > n {
                break;
            }
            r *= factor;
        }
        return r;
    }
    gamma(n + 1.0)
}

/// Human-friendly number formatting for the display + history.
pub fn format_number(v: f64) -> String {
    if v.is_nan() {
        return "Error".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "∞".into() } else { "-∞".into() };
    }
    if v == 0.0 {
        return "0".to_string();
    }
    let a = v.abs();
    if !(1e-6..1e12).contains(&a) {
        // Scientific, ~10 significant digits, tidy mantissa.
        let s = format!("{v:.6e}");
        return tidy_sci(&s);
    }
    let s = format!("{v:.10}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn tidy_sci(s: &str) -> String {
    // `1.234500e3` → `1.2345e3`
    if let Some((mant, exp)) = s.split_once('e') {
        let mant = mant.trim_end_matches('0').trim_end_matches('.');
        format!("{mant}e{exp}")
    } else {
        s.to_string()
    }
}

/// Replace ASCII operators / function names with their pretty unicode
/// forms for the expression display line. The stored expression stays
/// ASCII so the parser never sees unicode.
pub fn prettify(expr: &str) -> String {
    let mut s = expr.to_string();
    s = s.replace("sqrt(", "√(");
    s = s.replace("pi", "π");
    s = s.replace('*', "×");
    s = s.replace('/', "÷");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> f64 {
        evaluate(s, &Ctx::default()).unwrap()
    }
    fn evd(s: &str) -> f64 {
        evaluate(
            s,
            &Ctx {
                deg: true,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "these results are small integers, exact in f64 — an approximate\n                  compare here would stop proving the operator precedence"
    )]
    fn arithmetic_precedence() {
        assert_eq!(ev("1+2*3"), 7.0);
        assert_eq!(ev("(1+2)*3"), 9.0);
        assert_eq!(ev("2^3^2"), 512.0); // right assoc
        assert_eq!(ev("-2^2"), -4.0); // unary looser than power
        assert!((ev("10/4") - 2.5).abs() < 1e-12);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "these results are small integers, exact in f64 — an approximate\n                  compare here would stop proving implicit multiplication"
    )]
    fn implicit_multiplication() {
        assert!((ev("2pi") - 2.0 * PI).abs() < 1e-9);
        assert_eq!(ev("3(1+1)"), 6.0);
    }

    #[test]
    fn functions_and_constants() {
        assert!((ev("sqrt(16)") - 4.0).abs() < 1e-12);
        assert!((ev("ln(e)") - 1.0).abs() < 1e-12);
        assert!((ev("log(2,8)") - 3.0).abs() < 1e-12);
        assert!((ev("max(1,7,3)") - 7.0).abs() < 1e-12);
        assert!((ev("mod(7,3)") - 1.0).abs() < 1e-12);
    }

    #[test]
    fn degrees_vs_radians() {
        assert!((evd("sin(90)") - 1.0).abs() < 1e-9);
        assert!((ev("sin(pi/2)") - 1.0).abs() < 1e-9);
        assert!((evd("asin(1)") - 90.0).abs() < 1e-9);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "these results are small integers, exact in f64 — an approximate\n                  compare here would stop proving that 5! is exactly 120"
    )]
    fn factorial_and_percent() {
        assert_eq!(ev("5!"), 120.0);
        assert!((ev("50%") - 0.5).abs() < 1e-12);
        assert!((ev("200*50%") - 100.0).abs() < 1e-12);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "these results are small integers, exact in f64 — an approximate\n                  compare here would stop proving that x^2 samples exactly"
    )]
    fn compiled_sampling() {
        let c = compile("x^2", false, false).unwrap();
        assert_eq!(c.eval1(3.0), 9.0);
        let s = compile("sin(sqrt(x^2+y^2))", false, true).unwrap();
        assert!(s.eval2(0.0, 0.0).abs() < 1e-12);
    }

    #[test]
    fn errors_are_reported() {
        assert!(evaluate("1+", &Ctx::default()).is_err());
        assert!(evaluate("frobnicate(2)", &Ctx::default()).is_err());
        assert!(compile("z+1", false, false).is_err());
    }

    #[test]
    fn formatting() {
        assert_eq!(format_number(2.0), "2");
        assert_eq!(format_number(0.5), "0.5");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(f64::NAN), "Error");
    }
}
