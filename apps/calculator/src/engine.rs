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
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                // Scientific exponent: `e`/`E` followed by optional sign + digit.
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        i = j;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let s: String = chars[start..i].iter().collect();
                let v: f64 = s.parse().map_err(|_| format!("bad number `{s}`"))?;
                out.push(Tok::Num(v));
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Tok::Ident(s.to_ascii_lowercase()));
            }
            // Unicode niceties accepted in case they leak in from a paste.
            'π' => {
                out.push(Tok::Ident("pi".into()));
                i += 1;
            }
            '×' => {
                out.push(Tok::Op('*'));
                i += 1;
            }
            '÷' => {
                out.push(Tok::Op('/'));
                i += 1;
            }
            '−' => {
                out.push(Tok::Op('-'));
                i += 1;
            }
            '√' => {
                out.push(Tok::Ident("sqrt".into()));
                i += 1;
            }
            '+' | '-' | '*' | '/' | '^' | '!' | '%' => {
                out.push(Tok::Op(c));
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            _ => return Err(format!("unexpected `{c}`")),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Parser (precedence climbing)
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_bp(&mut self, min_bp: u8) -> Result<Expr, String> {
        let mut lhs = self.parse_prefix()?;
        loop {
            let tok = match self.peek() {
                Some(t) => t.clone(),
                None => break,
            };
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

/// Parse `input` into an AST. Returns a human-readable error on malformed
/// input (the UI shows it in the result line).
pub fn parse(input: &str) -> Result<Expr, String> {
    let toks = tokenize(input)?;
    if toks.is_empty() {
        return Err("empty".to_string());
    }
    let mut p = Parser { toks, pos: 0 };
    let e = p.parse_bp(0)?;
    if p.pos != p.toks.len() {
        return Err("trailing input".to_string());
    }
    Ok(e)
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

fn eval_node(e: &Expr, ctx: &Ctx) -> Result<f64, String> {
    Ok(match e {
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
        Expr::Neg(a) => -eval_node(a, ctx)?,
        Expr::Fact(a) => factorial(eval_node(a, ctx)?),
        Expr::Percent(a) => eval_node(a, ctx)? / 100.0,
        Expr::Bin(op, a, b) => {
            let x = eval_node(a, ctx)?;
            let y = eval_node(b, ctx)?;
            match op {
                '+' => x + y,
                '-' => x - y,
                '*' => x * y,
                '/' => x / y,
                '^' => x.powf(y),
                _ => return Err(format!("bad operator `{op}`")),
            }
        }
        Expr::Call(name, args) => eval_call(name, args, ctx)?,
    })
}

fn eval_call(name: &str, args: &[Expr], ctx: &Ctx) -> Result<f64, String> {
    let mut vals = Vec::with_capacity(args.len());
    for a in args {
        vals.push(eval_node(a, ctx)?);
    }
    let n = vals.len();
    let one = |f: fn(f64) -> f64| -> Result<f64, String> {
        if n != 1 {
            Err(format!("{name}() takes 1 argument"))
        } else {
            Ok(f(vals[0]))
        }
    };
    // Degree-aware trig: argument is degrees when `ctx.deg`.
    let to_rad = if ctx.deg { PI / 180.0 } else { 1.0 };
    let from_rad = if ctx.deg { 180.0 / PI } else { 1.0 };
    Ok(match name {
        "sin" => return one(|x| x).map(|x| (x * to_rad).sin()),
        "cos" => return one(|x| x).map(|x| (x * to_rad).cos()),
        "tan" => return one(|x| x).map(|x| (x * to_rad).tan()),
        "asin" => return one(|x| x).map(|x| x.asin() * from_rad),
        "acos" => return one(|x| x).map(|x| x.acos() * from_rad),
        "atan" => return one(|x| x).map(|x| x.atan() * from_rad),
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
        "rad" => return one(|x| x.to_radians()),
        "deg" => return one(|x| x.to_degrees()),
        "gamma" => return one(gamma),
        "fact" => return one(factorial),
        "log" => match n {
            1 => vals[0].log10(),
            2 => vals[1].log(vals[0]), // log(base, value)
            _ => return Err("log() takes 1 or 2 arguments".into()),
        },
        "root" => {
            if n != 2 {
                return Err("root(n, x) takes 2 arguments".into());
            }
            vals[1].powf(1.0 / vals[0])
        }
        "pow" => {
            if n != 2 {
                return Err("pow(a, b) takes 2 arguments".into());
            }
            vals[0].powf(vals[1])
        }
        "mod" => {
            if n != 2 {
                return Err("mod(a, b) takes 2 arguments".into());
            }
            vals[0].rem_euclid(vals[1])
        }
        "atan2" => {
            if n != 2 {
                return Err("atan2(y, x) takes 2 arguments".into());
            }
            vals[0].atan2(vals[1]) * from_rad
        }
        "hypot" => {
            if n != 2 {
                return Err("hypot(a, b) takes 2 arguments".into());
            }
            vals[0].hypot(vals[1])
        }
        "max" => {
            if n == 0 {
                return Err("max() needs arguments".into());
            }
            vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
        }
        "min" => {
            if n == 0 {
                return Err("min() needs arguments".into());
            }
            vals.iter().cloned().fold(f64::INFINITY, f64::min)
        }
        _ => return Err(format!("unknown function `{name}`")),
    })
}

/// Parse and evaluate `input` in one shot. Used by the `=` key.
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
        0.999_999_999_999_809_93,
        676.520_368_121_885_1,
        -1259.139_216_722_402_8,
        771.323_428_777_653_13,
        -176.615_029_162_140_59,
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
        let mut a = C[0];
        let t = x + G + 0.5;
        for (i, &coef) in C.iter().enumerate().skip(1) {
            a += coef / (x + i as f64);
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
        let mut r = 1.0f64;
        let mut i = 2.0f64;
        while i <= n {
            r *= i;
            i += 1.0;
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
    if a < 1e-6 || a >= 1e12 {
        // Scientific, ~10 significant digits, tidy mantissa.
        let s = format!("{:.6e}", v);
        return tidy_sci(&s);
    }
    let s = format!("{:.10}", v);
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
    fn arithmetic_precedence() {
        assert_eq!(ev("1+2*3"), 7.0);
        assert_eq!(ev("(1+2)*3"), 9.0);
        assert_eq!(ev("2^3^2"), 512.0); // right assoc
        assert_eq!(ev("-2^2"), -4.0); // unary looser than power
        assert!((ev("10/4") - 2.5).abs() < 1e-12);
    }

    #[test]
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
    fn factorial_and_percent() {
        assert_eq!(ev("5!"), 120.0);
        assert!((ev("50%") - 0.5).abs() < 1e-12);
        assert!((ev("200*50%") - 100.0).abs() < 1e-12);
    }

    #[test]
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
