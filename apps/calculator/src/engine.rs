//! Expression engine for the calculator.
//!
//! A compact, dependency-free math evaluator: tokenizer → Pratt parser →
//! AST → tree-walking evaluator. We own this rather than pulling a crate so
//! the build stays green offline AND so we get exact control over the
//! features a graphing calculator actually needs:
//!
//!   * a **degrees/radians** toggle that affects `sin/cos/tan` and their
//!     inverses (the popular crates hard-wire trig to radians), and
//!   * a **compiled** form that binds `x` (and `x,y`) so the 2D/3D plotters
//!     can sample a parsed expression thousands of times without re-parsing,
//!     and
//!   * **definite integration** with `int(expr,x,a,b)` (or `∫`), scoped `x`/`y`
//!     bindings, and bounded adaptive quadrature over finite intervals.
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
            '∫' => out.push(Tok::Ident("int".into())),
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

/// Reserved operands cannot be function names: `pi(2)` and `x(x+1)` are
/// implicit products, while an identifier such as `sin` introduces a call.
fn is_value_name(name: &str) -> bool {
    matches!(
        name,
        "pi" | "tau" | "e" | "x" | "y" | "ans" | "m" | "mem" | "inf" | "nan"
    )
}

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
        // This reaches the result line verbatim while a person is typing. A
        // next-step hint is more useful than the parser-internal "unexpected
        // end" for an unfinished call, parenthesis, or operator.
        let tok = self.next().ok_or_else(|| "enter a value".to_string())?;
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
                if matches!(self.peek(), Some(Tok::LParen)) && !is_value_name(&name) {
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
    // Nested integrals share this budget rather than multiplying independent
    // limits and potentially blocking the calculator for unbounded work.
    eval_with_budget(expr, ctx, &mut 32_768)
}

fn eval_with_budget(expr: &Expr, ctx: &Ctx, budget: &mut usize) -> Result<f64, String> {
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
        Expr::Neg(inner) => -eval_with_budget(inner, ctx, budget)?,
        Expr::Fact(inner) => factorial(eval_with_budget(inner, ctx, budget)?),
        Expr::Percent(inner) => eval_with_budget(inner, ctx, budget)? / 100.0,
        Expr::Bin(op, left, right) => {
            let lhs = eval_with_budget(left, ctx, budget)?;
            let rhs = eval_with_budget(right, ctx, budget)?;
            match op {
                '+' => lhs + rhs,
                '-' => lhs - rhs,
                '*' => lhs * rhs,
                '/' => lhs / rhs,
                '^' => lhs.powf(rhs),
                _ => return Err(format!("bad operator `{op}`")),
            }
        }
        Expr::Call(name, args) => eval_call(name, args, ctx, budget)?,
    })
}

fn eval_call(name: &str, args: &[Expr], ctx: &Ctx, budget: &mut usize) -> Result<f64, String> {
    // Integration binds an unevaluated expression, unlike ordinary functions.
    if name == "int" {
        return eval_integral(args, ctx, budget);
    }
    let mut vals = Vec::with_capacity(args.len());
    for arg in args {
        vals.push(eval_with_budget(arg, ctx, budget)?);
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
            // Fractional powf rejects a negative base even when an odd
            // integer root has a real solution. Evaluate its magnitude first.
            if radicand < 0.0 && degree.is_finite() && degree.fract() == 0.0 && degree % 2.0 != 0.0
            {
                -(-radicand).powf(1.0 / degree)
            } else {
                radicand.powf(1.0 / degree)
            }
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

struct Integral<'a> {
    expr: &'a Expr,
    variable: &'a str,
    lower: &'a Expr,
    upper: &'a Expr,
}

fn integral_args(args: &[Expr]) -> Result<Integral<'_>, String> {
    let [expr, variable, lower, upper] = args else {
        return Err("int(expr, x, a, b) takes 4 arguments".into());
    };
    let Expr::Var(variable) = variable else {
        return Err("integration variable must be x or y".into());
    };
    if !matches!(variable.as_str(), "x" | "y") {
        return Err("integration variable must be x or y".into());
    }
    Ok(Integral {
        expr,
        variable,
        lower,
        upper,
    })
}

fn eval_integral(args: &[Expr], ctx: &Ctx, budget: &mut usize) -> Result<f64, String> {
    let integral = integral_args(args)?;
    // Both bounds see the caller's bindings, even when this integral shadows
    // the same variable. Each sample gets its own copy, so nothing leaks out.
    let lower = eval_with_budget(integral.lower, ctx, budget)?;
    let upper = eval_with_budget(integral.upper, ctx, budget)?;
    if !lower.is_finite() || !upper.is_finite() {
        return Err("integration bounds must be finite".into());
    }
    if (upper - lower).abs() == 0.0 {
        return Ok(0.0);
    }
    let mut sample = |value| {
        if *budget == 0 {
            return Err("integration did not converge: sampling limit reached".into());
        }
        *budget -= 1;
        let mut bound = *ctx;
        if integral.variable == "x" {
            bound.x = value;
        } else {
            bound.y = value;
        }
        let result = eval_with_budget(integral.expr, &bound, budget)?;
        if !result.is_finite() {
            return Err(format!(
                "nonfinite integrand at {}={value}",
                integral.variable
            ));
        }
        Ok(result)
    };
    adaptive_integral([lower, upper], &mut sample)
}

/// A cached Simpson panel; halving its tolerance apportions the absolute
/// error allowance between its children without resampling their endpoints.
#[derive(Clone, Copy)]
struct IntegralPanel {
    bounds: [f64; 2],
    samples: [f64; 3],
    estimate: f64,
    tolerance: f64,
    depth: u8,
}

fn simpson(bounds: [f64; 2], samples: [f64; 3]) -> Result<f64, String> {
    let [a, b] = bounds;
    let [left, middle, right] = samples;
    let result = (b - a) * (left / 6.0 + middle * (2.0 / 3.0) + right / 6.0);
    if !result.is_finite() {
        return Err("integration overflow: use smaller bounds or integrand values".into());
    }
    Ok(result)
}

/// Adaptive Simpson quadrature with absolute and relative tolerance of 1e-9,
/// a maximum of 20 subdivisions along any branch, and a shared sample limit.
/// Two nonuniform initial panels avoid the dyadic aliasing that can make
/// oscillatory functions look constant at a single panel's five samples.
/// Endpoint singularities and other sampled nonfinite values are rejected;
/// this evaluates proper finite integrals, not improper integrals or limits.
fn adaptive_integral(
    bounds: [f64; 2],
    sample: &mut impl FnMut(f64) -> Result<f64, String>,
) -> Result<f64, String> {
    let [a, b] = bounds;
    // An irrational split keeps the two panels off the same periodic grid;
    // merely requiring more uniform subdivisions still aliases higher harmonics.
    let fraction = (5.0f64.sqrt() - 1.0) / 2.0;
    let split = a * (1.0 - fraction) + b * fraction;
    let left = sample(a)?;
    let middle = sample(split)?;
    let right = sample(b)?;
    let left_samples = [left, sample(a.midpoint(split))?, middle];
    let right_samples = [middle, sample(split.midpoint(b))?, right];
    let left_estimate = simpson([a, split], left_samples)?;
    let right_estimate = simpson([split, b], right_samples)?;
    let estimate = left_estimate + right_estimate;
    if !estimate.is_finite() {
        return Err("integration overflow: use smaller bounds or integrand values".into());
    }
    let tolerance = 1e-9 * estimate.abs().max(1.0) / 2.0;
    let mut result = 0.0;
    for (bounds, samples, estimate) in [
        ([a, split], left_samples, left_estimate),
        ([split, b], right_samples, right_estimate),
    ] {
        result += refine_integral(
            IntegralPanel {
                bounds,
                samples,
                estimate,
                tolerance,
                depth: 20,
            },
            sample,
        )?;
    }
    if !result.is_finite() {
        return Err("integration overflow: use smaller bounds or integrand values".into());
    }
    Ok(result)
}

fn refine_integral(
    panel: IntegralPanel,
    sample: &mut impl FnMut(f64) -> Result<f64, String>,
) -> Result<f64, String> {
    let [a, b] = panel.bounds;
    let [left, middle, right] = panel.samples;
    let midpoint = a.midpoint(b);
    let left_samples = [left, sample(a.midpoint(midpoint))?, middle];
    let right_samples = [middle, sample(midpoint.midpoint(b))?, right];
    let left_estimate = simpson([a, midpoint], left_samples)?;
    let right_estimate = simpson([midpoint, b], right_samples)?;
    let refined = left_estimate + right_estimate;
    let delta = refined - panel.estimate;
    if delta.abs() <= 15.0 * panel.tolerance {
        let result = refined + delta / 15.0;
        if result.is_finite() {
            return Ok(result);
        }
    }
    if panel.depth == 0 || midpoint <= a.min(b) || midpoint >= a.max(b) || !refined.is_finite() {
        return Err(
            "integration did not converge: subdivide the interval or simplify the integrand".into(),
        );
    }
    let left_result = refine_integral(
        IntegralPanel {
            bounds: [a, midpoint],
            samples: left_samples,
            estimate: left_estimate,
            tolerance: panel.tolerance / 2.0,
            depth: panel.depth - 1,
        },
        sample,
    )?;
    let right_result = refine_integral(
        IntegralPanel {
            bounds: [midpoint, b],
            samples: right_samples,
            estimate: right_estimate,
            tolerance: panel.tolerance / 2.0,
            depth: panel.depth - 1,
        },
        sample,
    )?;
    let result = left_result + right_result;
    if !result.is_finite() {
        return Err("integration overflow: use smaller bounds or integrand values".into());
    }
    Ok(result)
}

/// Parse and evaluate `input` in one shot. Used by the `=` key.
///
/// # Errors
/// The parse error for malformed input, or an evaluation error: an unknown
/// name or function, or a call with the wrong number of arguments. A domain
/// problem is normally a value, not an error — `1/0` evaluates to infinity.
/// Integration instead rejects nonfinite bounds/integrand values and reports
/// an error when its bounded adaptive quadrature cannot converge.
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

/// Parse + structurally validate an expression for plotting without evaluating
/// it. Domain failures, including nonfinite integrands or bounds, belong to
/// individual samples and become NaN rather than rejecting the whole plot.
///
/// # Errors
/// The parse error for malformed input, a variable the plot cannot supply
/// (`y` in a 2D plot, or any typo), or an unknown function / wrong arity.
pub fn compile(input: &str, deg: bool, two_var: bool) -> Result<Compiled, String> {
    let ast = parse(input)?;
    validate(&ast, two_var)?;
    Ok(Compiled { ast, deg })
}

/// Reject references to variables a plot can't supply (so a typo like
/// `z` in a 2D plot is reported instead of silently erroring per-sample).
fn validate(e: &Expr, two_var: bool) -> Result<(), String> {
    match e {
        Expr::Num(_) => Ok(()),
        Expr::Var(name) if is_value_name(name) && (two_var || name != "y") => Ok(()),
        Expr::Var(name) => Err(format!("unknown name `{name}`")),
        Expr::Neg(a) | Expr::Fact(a) | Expr::Percent(a) => validate(a, two_var),
        Expr::Bin(_, a, b) => {
            validate(a, two_var)?;
            validate(b, two_var)
        }
        Expr::Call(name, args) if name == "int" => {
            let integral = integral_args(args)?;
            validate(integral.lower, two_var)?;
            validate(integral.upper, two_var)?;
            validate(integral.expr, two_var || integral.variable == "y")
        }
        Expr::Call(name, args) => {
            validate_function(name, args.len())?;
            for a in args {
                validate(a, two_var)?;
            }
            Ok(())
        }
    }
}

/// Validate ordinary calls without evaluating their arguments: a domain
/// failure in a nested integral must not hide a misspelled function or arity.
fn validate_function(name: &str, count: usize) -> Result<(), String> {
    let (valid, expected) = match name {
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sinh" | "cosh" | "tanh" | "asinh"
        | "acosh" | "atanh" | "exp" | "ln" | "log10" | "log2" | "sqrt" | "cbrt" | "abs"
        | "sign" | "sgn" | "floor" | "ceil" | "round" | "trunc" | "fract" | "rad" | "deg"
        | "gamma" | "fact" => (count == 1, "1 argument"),
        "root" | "pow" | "mod" | "atan2" | "hypot" => (count == 2, "2 arguments"),
        "log" => ((1..=2).contains(&count), "1 or 2 arguments"),
        "min" | "max" => (count > 0, "at least 1 argument"),
        _ => return Err(format!("unknown function `{name}`")),
    };
    if valid {
        Ok(())
    } else {
        Err(format!("{name}() takes {expected}"))
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
        // Combine the power and exponential in log space: the power alone
        // overflows well before gamma's result exceeds f64's finite range.
        ((x + 0.5) * t.ln() - t + (TAU.sqrt() * a).ln()).exp()
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
    superscript_powers(&s)
}

/// `x^2` → `x²`: a `^` followed by a plain integer exponent (optionally
/// negative) becomes superscript digits. A power with a bracketed or symbolic
/// exponent keeps its `^`, since it has no single-glyph form.
fn superscript_powers(expr: &str) -> String {
    const DIGITS: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    let mut out = String::with_capacity(expr.len());
    let mut chars = expr.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '^' {
            out.push(c);
            continue;
        }
        let mut exponent = String::new();
        if chars.peek() == Some(&'-') {
            exponent.push('⁻');
            chars.next();
        }
        let mut digits = 0;
        while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
            exponent.extend(DIGITS.get(d as usize));
            digits += 1;
            chars.next();
        }
        if digits == 0 {
            out.push('^');
            if exponent.starts_with('⁻') {
                out.push('-');
            }
        } else {
            out.push_str(&exponent);
        }
    }
    out
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

    /// How a case's result is compared: `Exact` for small integers that are
    /// exact in f64 (an approximate compare would stop proving e.g. operator
    /// precedence or that 5! is exactly 120), `Near(tol)` otherwise.
    #[derive(Clone, Copy)]
    enum Cmp {
        Exact,
        Near(f64),
    }

    fn check(case: &str, got: f64, want: f64, cmp: Cmp) {
        match cmp {
            Cmp::Exact => assert_eq!(got, want, "case {case}"),
            Cmp::Near(tol) => assert!(
                (got - want).abs() < tol,
                "case {case}: got {got}, want {want} (tol {tol})"
            ),
        }
    }

    /// Evaluation (radians and degrees) and compiled sampling produce the
    /// expected values: precedence, implicit multiplication, functions and
    /// constants, angle modes, factorial and percent.
    #[test]
    fn evaluation_and_compiled_sampling_produce_expected_values() {
        use Cmp::{Exact, Near};
        // (group, source, degrees?, expected, comparison)
        let cases: &[(&str, &str, bool, f64, Cmp)] = &[
            // arithmetic_precedence
            ("arithmetic_precedence", "1+2*3", false, 7.0, Exact),
            ("arithmetic_precedence", "(1+2)*3", false, 9.0, Exact),
            ("arithmetic_precedence", "2^3^2", false, 512.0, Exact), // right assoc
            ("arithmetic_precedence", "-2^2", false, -4.0, Exact),   // unary looser than power
            ("arithmetic_precedence", "10/4", false, 2.5, Near(1e-12)),
            // implicit_multiplication
            (
                "implicit_multiplication",
                "2pi",
                false,
                2.0 * PI,
                Near(1e-9),
            ),
            ("implicit_multiplication", "3(1+1)", false, 6.0, Exact),
            // functions_and_constants
            (
                "functions_and_constants",
                "sqrt(16)",
                false,
                4.0,
                Near(1e-12),
            ),
            ("functions_and_constants", "ln(e)", false, 1.0, Near(1e-12)),
            (
                "functions_and_constants",
                "log(2,8)",
                false,
                3.0,
                Near(1e-12),
            ),
            (
                "functions_and_constants",
                "max(1,7,3)",
                false,
                7.0,
                Near(1e-12),
            ),
            (
                "functions_and_constants",
                "mod(7,3)",
                false,
                1.0,
                Near(1e-12),
            ),
            // degrees_vs_radians
            ("degrees_vs_radians", "sin(90)", true, 1.0, Near(1e-9)),
            ("degrees_vs_radians", "sin(pi/2)", false, 1.0, Near(1e-9)),
            ("degrees_vs_radians", "asin(1)", true, 90.0, Near(1e-9)),
            // factorial_and_percent
            ("factorial_and_percent", "5!", false, 120.0, Exact),
            ("factorial_and_percent", "50%", false, 0.5, Near(1e-12)),
            (
                "factorial_and_percent",
                "200*50%",
                false,
                100.0,
                Near(1e-12),
            ),
        ];
        for &(group, src, deg, want, cmp) in cases {
            let got = if deg { evd(src) } else { ev(src) };
            check(&format!("{group}: {src} (deg={deg})"), got, want, cmp);
        }

        // compiled_sampling
        let c = compile("x^2", false, false).unwrap();
        check("compiled_sampling: x^2 at 3", c.eval1(3.0), 9.0, Exact);
        let s = compile("sin(sqrt(x^2+y^2))", false, true).unwrap();
        assert!(
            s.eval2(0.0, 0.0).abs() < 1e-12,
            "compiled_sampling: surface at origin"
        );
    }

    #[test]
    fn values_followed_by_parentheses_use_implicit_multiplication() {
        let ctx = Ctx {
            x: 2.0,
            y: 3.0,
            ans: 4.0,
            mem: 5.0,
            ..Default::default()
        };
        for (source, expected, cmp) in [
            ("pi(2)", 2.0 * PI, Cmp::Near(1e-12)),
            ("π(2)", 2.0 * PI, Cmp::Near(1e-12)),
            ("tau(2)", 2.0 * TAU, Cmp::Near(1e-12)),
            ("e(2)", 2.0 * E, Cmp::Near(1e-12)),
            ("x(x+1)", 6.0, Cmp::Exact),
            ("y(2)", 6.0, Cmp::Exact),
            ("ans(2)", 8.0, Cmp::Exact),
            ("m(2)", 10.0, Cmp::Exact),
            ("mem(2)", 10.0, Cmp::Exact),
        ] {
            check(source, evaluate(source, &ctx).unwrap(), expected, cmp);
        }
        assert!(ev("inf(2)").is_infinite());
        assert!(ev("nan(2)").is_nan());
        let curve = compile("x(x+1)", false, false).unwrap();
        check("parenthetic curve", curve.eval1(2.0), 6.0, Cmp::Exact);
        let surface = compile("x(y+1)", false, true).unwrap();
        check(
            "parenthetic surface",
            surface.eval2(2.0, 3.0),
            8.0,
            Cmp::Exact,
        );
        assert!(compile("y(x+1)", false, false).is_err());
    }

    #[test]
    fn negative_radicands_have_real_odd_integer_roots() {
        for (source, expected) in [
            ("root(3,-8)", -2.0),
            ("root(5,-32)", -2.0),
            ("root(-3,-8)", -0.5),
            ("root(1,-8)", -8.0),
        ] {
            check(source, ev(source), expected, Cmp::Near(1e-12));
        }
        for source in ["root(2,-8)", "root(2.5,-8)"] {
            assert!(ev(source).is_nan(), "{source} has no real root");
        }
        let curve = compile("root(3,x)", false, false).unwrap();
        check("odd-root graph", curve.eval1(-8.0), -2.0, Cmp::Near(1e-12));
        let surface = compile("root(3,x+y)", false, true).unwrap();
        check(
            "odd-root surface",
            surface.eval2(-3.0, -5.0),
            -2.0,
            Cmp::Near(1e-12),
        );
    }

    #[test]
    fn gamma_preserves_representable_large_results() {
        for n in [140u32, 150, 170, 171] {
            let source = format!("gamma({n})");
            let expected = factorial(f64::from(n - 1));
            let got = ev(&source);
            assert!(got.is_finite(), "{source} overflowed: {got}");
            assert!(
                (got / expected - 1.0).abs() < 1e-12,
                "{source}: got {got}, expected {expected}",
            );
        }
        // Half-integer recurrence is independent of the Lanczos formula and
        // also exercises the generalized factorial path outside integer inputs.
        let expected = (0..150u32).fold(PI.sqrt(), |value, n| value * (f64::from(n) + 0.5));
        for source in ["gamma(150.5)", "149.5!", "fact(149.5)"] {
            let got = ev(source);
            assert!(
                (got / expected - 1.0).abs() < 1e-12,
                "{source}: got {got}, expected {expected}",
            );
        }
        check("gamma(0.5)", ev("gamma(0.5)"), PI.sqrt(), Cmp::Near(1e-12));
        check(
            "gamma(-0.5)",
            ev("gamma(-0.5)"),
            -2.0 * PI.sqrt(),
            Cmp::Near(1e-12),
        );
        let curve = compile("gamma(x)", false, false).unwrap();
        assert!(curve.eval1(150.0).is_finite());
        assert!(ev("gamma(172)").is_infinite());
        assert!(ev("171!").is_infinite());
    }

    #[test]
    fn definite_integrals_cover_orientation_alias_and_adaptive_sampling() {
        for (source, expected) in [
            ("int(x^2,x,0,3)", 9.0),
            ("int(x^2,x,3,0)", -9.0),
            ("int(x,x,-2,2)", 0.0),
            ("int(1/x,x,0,0)", 0.0),
            ("int(x,x,2,2)", 0.0),
            ("∫(x^2,x,0,1)", 1.0 / 3.0),
            ("2int(x,x,0,1)+1", 2.0),
            ("int(sin(x),x,0,pi)", 2.0),
            ("int(exp(x),x,0,1)", E - 1.0),
            (
                "int(1/(1+10000*(x-0.3)^2),x,0,1)",
                (70.0f64.atan() + 30.0f64.atan()) / 100.0,
            ),
        ] {
            check(source, ev(source), expected, Cmp::Near(1e-8));
        }
        check(
            "degree-mode integral",
            evd("int(sin(x),x,0,180)"),
            360.0 / PI,
            Cmp::Near(1e-8),
        );
    }

    #[test]
    fn integral_initial_samples_do_not_alias_periodic_functions() {
        // A single Simpson panel and its first refinement sample only maxima
        // of these cosines, falsely reporting the integral as one.
        for frequency in [8, 16, 32, 64] {
            for (source, expected) in [
                (format!("int(cos({frequency}*pi*x),x,0,1)"), 0.0),
                (format!("int(1+cos({frequency}*pi*x),x,1,0)"), -1.0),
                (format!("int(sin({frequency}*pi*x)^2,x,0,1)"), 0.5),
            ] {
                check(&source, ev(&source), expected, Cmp::Near(1e-8));
            }
        }
        check(
            "degree-mode periodic alias",
            evd("int(cos(1440*x),x,0,1)"),
            0.0,
            Cmp::Near(1e-8),
        );
    }

    #[test]
    fn integral_bindings_are_scoped_and_bounds_use_the_outer_context() {
        let ctx = Ctx {
            x: 4.0,
            y: 3.0,
            ans: 2.0,
            mem: 5.0,
            ..Default::default()
        };
        for (source, expected) in [
            ("int(x+y+ans+m,x,0,2)+x+y", 29.0),
            ("int(x,x,0,x)+x", 12.0),
            ("int(y,y,0,y)+y", 7.5),
            ("int(int(x+y,y,0,x),x,0,1)", 0.5),
            ("int(int(x,x,0,x)+x,x,0,1)", 2.0 / 3.0),
            ("int(int(y,y,0,y)+y,y,0,1)", 2.0 / 3.0),
            ("int(int(x*y,x,0,y),y,0,1)", 0.125),
            ("int(int(x,x,0,1)+x,x,0,1)", 1.0),
        ] {
            check(
                source,
                evaluate(source, &ctx).unwrap(),
                expected,
                Cmp::Near(1e-9),
            );
        }
        // Angle mode and saved values survive both levels of binding.
        check(
            "nested degree context",
            evd("int(int(sin(x)+cos(y),y,0,90),x,0,90)"),
            32_400.0 / PI,
            Cmp::Near(1e-6),
        );
    }

    #[test]
    fn compiled_integrals_distinguish_bound_and_free_variables() {
        let curve = compile("int(y^2,y,0,x)", false, false).unwrap();
        check(
            "bound y in curve",
            curve.eval1(2.0),
            8.0 / 3.0,
            Cmp::Near(1e-9),
        );
        let nested = compile("int(int(x*y,x,0,y),y,0,x)", false, false).unwrap();
        check(
            "nested bindings in curve",
            nested.eval1(2.0),
            2.0,
            Cmp::Near(1e-9),
        );
        let surface = compile("int(x*y,x,0,1)+x", false, true).unwrap();
        check(
            "integral in surface",
            surface.eval2(2.0, 3.0),
            3.5,
            Cmp::Near(1e-9),
        );
        for source in [
            "int(y,y,0,y)",
            "int(y,y,0,1)+y",
            "int(int(y,y,0,1)+y,x,0,1)",
            "int(z,x,0,1)",
        ] {
            assert!(compile(source, false, false).is_err(), "{source}");
        }
    }

    #[test]
    fn compiled_integrals_allow_sample_dependent_domain_failures() {
        let curve = compile("int(sqrt(x-y),y,0,1)", false, false).unwrap();
        assert!(curve.eval1(0.7).is_nan());
        check(
            "integral valid away from the former compile probe",
            curve.eval1(2.0),
            (2.0f64.powf(1.5) - 1.0) * 2.0 / 3.0,
            Cmp::Near(1e-9),
        );
        let bounds = compile("int(y,y,0,1/x)", false, false).unwrap();
        assert!(bounds.eval1(0.0).is_nan());
        check(
            "finite sample bounds",
            bounds.eval1(2.0),
            0.125,
            Cmp::Near(1e-9),
        );
        let surface = compile("int(sqrt(x-y),x,0,1)", false, true).unwrap();
        assert!(surface.eval2(0.0, 0.3).is_nan());
        assert!(surface.eval2(0.0, -1.0).is_finite());
    }

    #[test]
    fn compiled_calls_are_validated_without_evaluating_integrals() {
        // A zero-width integral never samples its body. Unknown functions and
        // malformed calls must still fail compilation, even inside that body.
        for source in [
            "int(typo(x),x,0,0)",
            "int(sqrt(),x,0,0)",
            "int(pow(x),x,0,0)",
            "int(log(1,2,3),x,0,0)",
            "int(min(),x,0,0)",
            "int(max(),x,0,0)",
            "int(int(x,x,0),x,0,0)",
            "int(int(x,z,0,1),x,0,0)",
            "typo(int(sqrt(x-y),y,0,1))",
            "sqrt(int(sqrt(x-y),y,0,1),2)",
        ] {
            assert!(compile(source, false, false).is_err(), "{source}");
        }
        for source in [
            "int(log(2),x,0,0)",
            "int(log(2,8),x,0,0)",
            "int(min(x,1,2),x,0,0)",
            "int(max(x),x,0,0)",
        ] {
            assert!(compile(source, false, false).is_ok(), "{source}");
        }
    }

    #[test]
    fn integrals_report_malformed_calls_and_nonfinite_values() {
        for source in [
            "int()",
            "int(x)",
            "int(x,x,0)",
            "int(x,x,0,1,2)",
            "int(x,x,0,)",
            "int(x,x,0,1",
            "∫(x,x,,1)",
        ] {
            assert!(evaluate(source, &Ctx::default()).is_err(), "{source}");
        }
        for variable in ["z", "pi", "ans", "1", "x+0", "sin(x)"] {
            let source = format!("int(x,{variable},0,1)");
            assert_eq!(
                evaluate(&source, &Ctx::default()),
                Err("integration variable must be x or y".into()),
                "{source}",
            );
        }
        for source in [
            "int(x,x,0,inf)",
            "int(x,x,nan,1)",
            "int(x,x,0,1/0)",
            "int(x,x,sqrt(-1),1)",
            "int(x,x,inf,inf)",
        ] {
            assert_eq!(
                evaluate(source, &Ctx::default()),
                Err("integration bounds must be finite".into()),
                "{source}",
            );
        }
        for source in ["int(1/x,x,0,1)", "int(sqrt(x),x,-1,1)", "int(nan,y,0,1)"] {
            let error = evaluate(source, &Ctx::default()).unwrap_err();
            assert!(error.contains("nonfinite integrand"), "{source}: {error}");
        }
    }

    #[test]
    fn integration_work_is_bounded_including_nested_calls() {
        let error = evaluate("int(floor(x),x,0,1)", &Ctx::default()).unwrap_err();
        assert!(error.contains("did not converge"), "{error}");
        let mut nested = "1".to_string();
        for _ in 0..7 {
            nested = format!("int({nested},x,0,1)");
        }
        let error = evaluate(&nested, &Ctx::default()).unwrap_err();
        assert!(error.contains("sampling limit reached"), "{error}");
    }

    /// What the result line shows a person: error messages and rejections for
    /// malformed input, and the display formatting of numbers.
    #[test]
    fn errors_and_number_formatting_render_user_facing_results() {
        // errors_are_reported
        assert_eq!(
            evaluate("sqrt(", &Ctx::default()),
            Err("enter a value".into()),
            "the live result line tells a person what to do next"
        );
        assert!(
            evaluate("1+", &Ctx::default()).is_err(),
            "errors: dangling operator"
        );
        assert!(
            evaluate("frobnicate(2)", &Ctx::default()).is_err(),
            "errors: unknown function"
        );
        assert!(
            compile("z+1", false, false).is_err(),
            "errors: unknown variable in compile"
        );

        // formatting
        for (x, want) in [
            (2.0, "2"),
            (0.5, "0.5"),
            (0.1 + 0.2, "0.3"),
            (f64::NAN, "Error"),
        ] {
            assert_eq!(format_number(x), want, "formatting case {x}");
        }
    }
}

#[cfg(test)]
mod prettify_tests {
    use super::prettify;

    /// A power with a plain integer exponent reads as a superscript; one with
    /// a bracketed or symbolic exponent keeps its caret.
    #[test]
    fn integer_powers_become_superscripts() {
        assert_eq!(prettify("2^10"), "2¹⁰");
        assert_eq!(prettify("x^2+1"), "x²+1");
        assert_eq!(prettify("2^-3"), "2⁻³");
        assert_eq!(prettify("2^(1/2)"), "2^(1÷2)");
        assert_eq!(prettify("2^x"), "2^x");
        assert_eq!(prettify("5^"), "5^");
        assert_eq!(prettify("2^-x"), "2^-x");
    }

    #[test]
    fn operators_and_functions_take_their_pretty_forms() {
        assert_eq!(prettify("12*(4+3)"), "12×(4+3)");
        assert_eq!(prettify("sqrt(2)/pi"), "√(2)÷π");
    }
}
