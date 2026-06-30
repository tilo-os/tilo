//! Formula engine: tokenizer -> recursive-descent parser -> evaluator.
//!
//! Supports a practical subset of the spreadsheet formula language:
//!   * literals: numbers, `"strings"`, `TRUE` / `FALSE`
//!   * cell refs `A1`, absolute `$A$1`, and ranges `A1:B3`
//!   * arithmetic `+ - * / ^` and unary `-`/`+`
//!   * text concatenation `&`
//!   * comparisons `= <> < > <= >=`
//!   * functions: SUM, AVERAGE, MIN, MAX, COUNT, COUNTA, PRODUCT, MEDIAN,
//!     IF, AND, OR, NOT, ABS, ROUND, ROUNDDOWN, ROUNDUP, INT, MOD, SQRT,
//!     POWER, FLOOR, CEILING, LEN, UPPER, LOWER, TRIM, LEFT, RIGHT, MID,
//!     CONCAT/CONCATENATE, SUMIF, COUNTIF, PI.
//!
//! Evaluation is memoised with on-the-fly cycle detection so a sheet with
//! `A1 = B1 + 1`, `B1 = A1` resolves to `#CYCLE!` rather than recursing
//! forever. References that point at unparseable cells yield `#REF!`;
//! division by zero yields `#DIV/0!`; type mismatches yield `#VALUE!`.

use std::collections::{HashMap, HashSet};

use crate::model::{a1_to_addr, Addr, CellFormat, CellValue, FormatCategory};

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Num(f64),
    Str(String),
    Ident(String), // function name or reference text
    Op(char),      // + - * / ^ & % ( ) , : and comparison head chars
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_digit() || chars[i] == '.')
                {
                    i += 1;
                }
                // optional scientific exponent: 1e3, 2.5E-4
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let save = i;
                    i += 1;
                    if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                        i += 1;
                    }
                    if i < chars.len() && chars[i].is_ascii_digit() {
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    } else {
                        i = save; // not actually an exponent
                    }
                }
                let s: String = chars[start..i].iter().collect();
                let n = s.parse::<f64>().map_err(|_| format!("bad number {s}"))?;
                out.push(Token::Num(n));
            }
            '"' => {
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != '"' {
                    s.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err("unterminated string".into());
                }
                i += 1; // closing quote
                out.push(Token::Str(s));
            }
            '$' | 'A'..='Z' | 'a'..='z' | '_' => {
                // Identifier: function name or cell reference (may contain $).
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric()
                        || chars[i] == '$'
                        || chars[i] == '_'
                        || chars[i] == '.')
                {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Token::Ident(s));
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    out.push(Token::Le);
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '>' {
                    out.push(Token::Ne);
                    i += 2;
                } else {
                    out.push(Token::Lt);
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    out.push(Token::Ge);
                    i += 2;
                } else {
                    out.push(Token::Gt);
                    i += 1;
                }
            }
            '=' => {
                out.push(Token::Eq);
                i += 1;
            }
            '+' | '-' | '*' | '/' | '^' | '&' | '%' | '(' | ')' | ','
            | ':' => {
                out.push(Token::Op(c));
                i += 1;
            }
            _ => return Err(format!("unexpected char '{c}'")),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// AST + parser
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Expr {
    Num(f64),
    Str(String),
    Bool(bool),
    Ref(Addr),
    Range(Addr, Addr),
    Unary(char, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse(&mut self) -> Result<Expr, String> {
        let e = self.parse_comparison()?;
        if self.pos != self.tokens.len() {
            return Err("trailing tokens after expression".into());
        }
        Ok(e)
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_concat()?;
        loop {
            let op = match self.peek() {
                Some(Token::Eq) => BinOp::Eq,
                Some(Token::Ne) => BinOp::Ne,
                Some(Token::Lt) => BinOp::Lt,
                Some(Token::Gt) => BinOp::Gt,
                Some(Token::Le) => BinOp::Le,
                Some(Token::Ge) => BinOp::Ge,
                _ => break,
            };
            self.next();
            let right = self.parse_concat()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_concat(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_add()?;
        while matches!(self.peek(), Some(Token::Op('&'))) {
            self.next();
            let right = self.parse_add()?;
            left = Expr::Binary(BinOp::Concat, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Some(Token::Op('+')) => BinOp::Add,
                Some(Token::Op('-')) => BinOp::Sub,
                _ => break,
            };
            self.next();
            let right = self.parse_mul()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Op('*')) => BinOp::Mul,
                Some(Token::Op('/')) => BinOp::Div,
                _ => break,
            };
            self.next();
            let right = self.parse_unary()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if let Some(Token::Op(c @ ('-' | '+'))) = self.peek().cloned() {
            self.next();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary(c, Box::new(e)));
        }
        self.parse_power()
    }

    fn parse_power(&mut self) -> Result<Expr, String> {
        let base = self.parse_postfix()?;
        if matches!(self.peek(), Some(Token::Op('^'))) {
            self.next();
            // Right-associative exponentiation.
            let exp = self.parse_unary()?;
            return Ok(Expr::Binary(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_primary()?;
        // Percent literal suffix: `50%` -> 0.5.
        while matches!(self.peek(), Some(Token::Op('%'))) {
            self.next();
            e = Expr::Binary(
                BinOp::Div,
                Box::new(e),
                Box::new(Expr::Num(100.0)),
            );
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Token::Num(n)) => Ok(Expr::Num(n)),
            Some(Token::Str(s)) => Ok(Expr::Str(s)),
            Some(Token::Op('(')) => {
                let e = self.parse_comparison()?;
                match self.next() {
                    Some(Token::Op(')')) => Ok(e),
                    _ => Err("expected ')'".into()),
                }
            }
            Some(Token::Ident(id)) => {
                // Function call?
                if matches!(self.peek(), Some(Token::Op('('))) {
                    self.next(); // consume '('
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some(Token::Op(')'))) {
                        loop {
                            args.push(self.parse_comparison()?);
                            match self.peek() {
                                Some(Token::Op(',')) => {
                                    self.next();
                                }
                                _ => break,
                            }
                        }
                    }
                    match self.next() {
                        Some(Token::Op(')')) => {}
                        _ => return Err("expected ')' after args".into()),
                    }
                    return Ok(Expr::Call(id.to_uppercase(), args));
                }
                // Boolean literals.
                match id.to_uppercase().as_str() {
                    "TRUE" => return Ok(Expr::Bool(true)),
                    "FALSE" => return Ok(Expr::Bool(false)),
                    _ => {}
                }
                // Range A1:B3 ?
                if matches!(self.peek(), Some(Token::Op(':'))) {
                    self.next();
                    let end = match self.next() {
                        Some(Token::Ident(e)) => e,
                        _ => return Err("expected ref after ':'".into()),
                    };
                    let a = a1_to_addr(&id).ok_or_else(|| format!("#REF: {id}"))?;
                    let b = a1_to_addr(&end).ok_or_else(|| format!("#REF: {end}"))?;
                    return Ok(Expr::Range(a, b));
                }
                // Single cell reference.
                let addr = a1_to_addr(&id).ok_or_else(|| format!("#NAME?: {id}"))?;
                Ok(Expr::Ref(addr))
            }
            other => Err(format!("unexpected token {other:?}")),
        }
    }
}

/// Parse a formula body (without the leading `=`) into an `Expr`. The
/// returned tree can be evaluated repeatedly against changing cell data.
fn parse(formula: &str) -> Result<Expr, String> {
    let tokens = tokenize(formula)?;
    if tokens.is_empty() {
        return Err("empty formula".into());
    }
    Parser::new(tokens).parse()
}

// ---------------------------------------------------------------------------
// Evaluation engine
// ---------------------------------------------------------------------------

/// The recalculation engine. Holds the raw input strings for every
/// non-empty cell and produces a fully-evaluated value map, recomputing
/// formulas with memoisation + cycle detection.
pub struct Engine<'a> {
    raw: &'a HashMap<Addr, String>,
    cache: HashMap<Addr, CellValue>,
    visiting: HashSet<Addr>,
}

impl<'a> Engine<'a> {
    pub fn new(raw: &'a HashMap<Addr, String>) -> Self {
        Self {
            raw,
            cache: HashMap::new(),
            visiting: HashSet::new(),
        }
    }

    /// Evaluate every populated cell and return the value map. Cells that
    /// hold plain numbers/text are parsed directly; cells beginning with
    /// `=` are parsed and evaluated.
    pub fn evaluate_all(mut self) -> HashMap<Addr, CellValue> {
        let addrs: Vec<Addr> = self.raw.keys().copied().collect();
        for addr in addrs {
            if !self.cache.contains_key(&addr) {
                let v = self.eval_cell(addr);
                self.cache.insert(addr, v);
            }
        }
        self.cache
    }

    /// Resolve a single cell, using/filling the memo cache.
    fn eval_cell(&mut self, addr: Addr) -> CellValue {
        if let Some(v) = self.cache.get(&addr) {
            return v.clone();
        }
        if self.visiting.contains(&addr) {
            return CellValue::Error("#CYCLE!".into());
        }
        let raw = match self.raw.get(&addr) {
            Some(r) if !r.is_empty() => r.clone(),
            _ => return CellValue::Empty,
        };

        let value = if let Some(body) = raw.strip_prefix('=') {
            self.visiting.insert(addr);
            let v = match parse(body) {
                Ok(expr) => self.eval_expr(&expr),
                Err(_) => CellValue::Error("#ERR!".into()),
            };
            self.visiting.remove(&addr);
            v
        } else {
            parse_literal(&raw)
        };
        self.cache.insert(addr, value.clone());
        value
    }

    fn eval_expr(&mut self, expr: &Expr) -> CellValue {
        match expr {
            Expr::Num(n) => CellValue::Number(*n),
            Expr::Str(s) => CellValue::Text(s.clone()),
            Expr::Bool(b) => CellValue::Bool(*b),
            Expr::Ref(addr) => self.eval_cell(*addr),
            Expr::Range(_, _) => CellValue::Error("#VALUE!".into()),
            Expr::Unary(op, inner) => {
                let v = self.eval_expr(inner);
                if v.is_error() {
                    return v;
                }
                match v.as_number() {
                    Some(n) => CellValue::Number(if *op == '-' { -n } else { n }),
                    None => CellValue::Error("#VALUE!".into()),
                }
            }
            Expr::Binary(op, l, r) => self.eval_binary(*op, l, r),
            Expr::Call(name, args) => self.eval_call(name, args),
        }
    }

    fn eval_binary(&mut self, op: BinOp, l: &Expr, r: &Expr) -> CellValue {
        let lv = self.eval_expr(l);
        let rv = self.eval_expr(r);
        if lv.is_error() {
            return lv;
        }
        if rv.is_error() {
            return rv;
        }
        if op == BinOp::Concat {
            return CellValue::Text(format!(
                "{}{}",
                lv.to_plain_string(),
                rv.to_plain_string()
            ));
        }
        // Comparisons can compare text or numbers.
        if matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge
        ) {
            return self.eval_compare(op, &lv, &rv);
        }
        let (a, b) = match (lv.as_number(), rv.as_number()) {
            (Some(a), Some(b)) => (a, b),
            _ => return CellValue::Error("#VALUE!".into()),
        };
        let result = match op {
            BinOp::Add => a + b,
            BinOp::Sub => a - b,
            BinOp::Mul => a * b,
            BinOp::Div => {
                if b == 0.0 {
                    return CellValue::Error("#DIV/0!".into());
                }
                a / b
            }
            BinOp::Pow => a.powf(b),
            _ => unreachable!(),
        };
        CellValue::Number(result)
    }

    fn eval_compare(&self, op: BinOp, lv: &CellValue, rv: &CellValue) -> CellValue {
        // Numeric comparison when both coerce; otherwise string comparison.
        let ord = match (lv.as_number(), rv.as_number()) {
            (Some(a), Some(b)) => a.partial_cmp(&b),
            _ => Some(lv.to_plain_string().cmp(&rv.to_plain_string())),
        };
        let Some(ord) = ord else {
            return CellValue::Error("#VALUE!".into());
        };
        use std::cmp::Ordering::*;
        let res = match op {
            BinOp::Eq => ord == Equal,
            BinOp::Ne => ord != Equal,
            BinOp::Lt => ord == Less,
            BinOp::Gt => ord == Greater,
            BinOp::Le => ord != Greater,
            BinOp::Ge => ord != Less,
            _ => unreachable!(),
        };
        CellValue::Bool(res)
    }

    /// Flatten an argument into the list of scalar values it represents.
    /// A `Range` expands to every cell it spans; everything else is a
    /// single value.
    fn collect_values(&mut self, arg: &Expr) -> Vec<CellValue> {
        match arg {
            Expr::Range(a, b) => {
                let (c0, c1) = (a.0.min(b.0), a.0.max(b.0));
                let (r0, r1) = (a.1.min(b.1), a.1.max(b.1));
                let mut out = Vec::new();
                for r in r0..=r1 {
                    for c in c0..=c1 {
                        out.push(self.eval_cell((c, r)));
                    }
                }
                out
            }
            _ => vec![self.eval_expr(arg)],
        }
    }

    /// Numeric values across every argument (ranges flattened). Used by
    /// the numeric aggregate functions. Non-numeric, non-empty cells are
    /// skipped (matching SUM/AVERAGE semantics); errors propagate.
    fn numeric_args(&mut self, args: &[Expr]) -> Result<Vec<f64>, CellValue> {
        let mut nums = Vec::new();
        for a in args {
            for v in self.collect_values(a) {
                if v.is_error() {
                    return Err(v);
                }
                match v {
                    CellValue::Number(n) => nums.push(n),
                    CellValue::Bool(b) => nums.push(if b { 1.0 } else { 0.0 }),
                    // Empty + text are skipped by SUM-family functions.
                    _ => {}
                }
            }
        }
        Ok(nums)
    }

    fn eval_call(&mut self, name: &str, args: &[Expr]) -> CellValue {
        match name {
            "SUM" => match self.numeric_args(args) {
                Ok(ns) => CellValue::Number(ns.iter().sum()),
                Err(e) => e,
            },
            "PRODUCT" => match self.numeric_args(args) {
                Ok(ns) => CellValue::Number(ns.iter().product()),
                Err(e) => e,
            },
            "AVERAGE" => match self.numeric_args(args) {
                Ok(ns) if !ns.is_empty() => {
                    CellValue::Number(ns.iter().sum::<f64>() / ns.len() as f64)
                }
                Ok(_) => CellValue::Error("#DIV/0!".into()),
                Err(e) => e,
            },
            "MEDIAN" => match self.numeric_args(args) {
                Ok(mut ns) if !ns.is_empty() => {
                    ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let mid = ns.len() / 2;
                    let m = if ns.len() % 2 == 0 {
                        (ns[mid - 1] + ns[mid]) / 2.0
                    } else {
                        ns[mid]
                    };
                    CellValue::Number(m)
                }
                Ok(_) => CellValue::Error("#NUM!".into()),
                Err(e) => e,
            },
            "MIN" => match self.numeric_args(args) {
                Ok(ns) => {
                    if ns.is_empty() {
                        CellValue::Number(0.0)
                    } else {
                        CellValue::Number(ns.iter().cloned().fold(f64::INFINITY, f64::min))
                    }
                }
                Err(e) => e,
            },
            "MAX" => match self.numeric_args(args) {
                Ok(ns) => {
                    if ns.is_empty() {
                        CellValue::Number(0.0)
                    } else {
                        CellValue::Number(ns.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
                    }
                }
                Err(e) => e,
            },
            "COUNT" => {
                // Count of numeric cells.
                let mut n = 0;
                for a in args {
                    for v in self.collect_values(a) {
                        if matches!(v, CellValue::Number(_)) {
                            n += 1;
                        }
                    }
                }
                CellValue::Number(n as f64)
            }
            "COUNTA" => {
                // Count of non-empty cells.
                let mut n = 0;
                for a in args {
                    for v in self.collect_values(a) {
                        if !v.is_empty() {
                            n += 1;
                        }
                    }
                }
                CellValue::Number(n as f64)
            }
            "IF" => {
                if args.len() < 2 {
                    return CellValue::Error("#ERR!".into());
                }
                let cond = self.eval_expr(&args[0]);
                if cond.is_error() {
                    return cond;
                }
                let truthy = match cond {
                    CellValue::Bool(b) => b,
                    CellValue::Number(n) => n != 0.0,
                    CellValue::Empty => false,
                    CellValue::Text(ref t) => !t.is_empty(),
                    CellValue::Error(_) => false,
                };
                if truthy {
                    self.eval_expr(&args[1])
                } else if args.len() >= 3 {
                    self.eval_expr(&args[2])
                } else {
                    CellValue::Bool(false)
                }
            }
            "AND" => {
                let vals = self.flatten_bools(args);
                match vals {
                    Ok(bs) => CellValue::Bool(bs.into_iter().all(|b| b)),
                    Err(e) => e,
                }
            }
            "OR" => {
                let vals = self.flatten_bools(args);
                match vals {
                    Ok(bs) => CellValue::Bool(bs.into_iter().any(|b| b)),
                    Err(e) => e,
                }
            }
            "NOT" => {
                if args.len() != 1 {
                    return CellValue::Error("#ERR!".into());
                }
                let v = self.eval_expr(&args[0]);
                match v.as_number() {
                    Some(n) => CellValue::Bool(n == 0.0),
                    None => CellValue::Error("#VALUE!".into()),
                }
            }
            "ABS" => self.unary_num(args, |n| n.abs()),
            "SQRT" => self.unary_num(args, |n| n.sqrt()),
            "INT" => self.unary_num(args, |n| n.floor()),
            "SIGN" => self.unary_num(args, |n| n.signum()),
            "EXP" => self.unary_num(args, |n| n.exp()),
            "LN" => self.unary_num(args, |n| n.ln()),
            "LOG10" => self.unary_num(args, |n| n.log10()),
            "PI" => CellValue::Number(std::f64::consts::PI),
            "ROUND" => self.round_fn(args, RoundMode::Nearest),
            "ROUNDDOWN" => self.round_fn(args, RoundMode::Down),
            "ROUNDUP" => self.round_fn(args, RoundMode::Up),
            "MOD" => self.binary_num(args, |a, b| {
                if b == 0.0 {
                    None
                } else {
                    Some(a - b * (a / b).floor())
                }
            }),
            "POWER" => self.binary_num(args, |a, b| Some(a.powf(b))),
            "FLOOR" => self.unary_num(args, |n| n.floor()),
            "CEILING" => self.unary_num(args, |n| n.ceil()),
            "LEN" => {
                let v = self.eval_expr(args.first().unwrap_or(&Expr::Str(String::new())));
                CellValue::Number(v.to_plain_string().chars().count() as f64)
            }
            "UPPER" => self.unary_text(args, |s| s.to_uppercase()),
            "LOWER" => self.unary_text(args, |s| s.to_lowercase()),
            "TRIM" => self.unary_text(args, |s| s.trim().to_string()),
            "LEFT" => self.substr_fn(args, true),
            "RIGHT" => self.substr_fn(args, false),
            "MID" => self.mid_fn(args),
            "CONCAT" | "CONCATENATE" => {
                let mut s = String::new();
                for a in args {
                    for v in self.collect_values(a) {
                        if v.is_error() {
                            return v;
                        }
                        s.push_str(&v.to_plain_string());
                    }
                }
                CellValue::Text(s)
            }
            "SUMIF" => self.sumif(args),
            "COUNTIF" => self.countif(args),
            _ => CellValue::Error("#NAME?".into()),
        }
    }

    fn flatten_bools(&mut self, args: &[Expr]) -> Result<Vec<bool>, CellValue> {
        let mut out = Vec::new();
        for a in args {
            for v in self.collect_values(a) {
                if v.is_error() {
                    return Err(v);
                }
                match v.as_number() {
                    Some(n) => out.push(n != 0.0),
                    None => {} // skip non-numeric
                }
            }
        }
        Ok(out)
    }

    fn unary_num(&mut self, args: &[Expr], f: impl Fn(f64) -> f64) -> CellValue {
        let Some(a) = args.first() else {
            return CellValue::Error("#ERR!".into());
        };
        let v = self.eval_expr(a);
        if v.is_error() {
            return v;
        }
        match v.as_number() {
            Some(n) => CellValue::Number(f(n)),
            None => CellValue::Error("#VALUE!".into()),
        }
    }

    fn binary_num(
        &mut self,
        args: &[Expr],
        f: impl Fn(f64, f64) -> Option<f64>,
    ) -> CellValue {
        if args.len() != 2 {
            return CellValue::Error("#ERR!".into());
        }
        let a = self.eval_expr(&args[0]);
        let b = self.eval_expr(&args[1]);
        if a.is_error() {
            return a;
        }
        if b.is_error() {
            return b;
        }
        match (a.as_number(), b.as_number()) {
            (Some(x), Some(y)) => match f(x, y) {
                Some(r) => CellValue::Number(r),
                None => CellValue::Error("#DIV/0!".into()),
            },
            _ => CellValue::Error("#VALUE!".into()),
        }
    }

    fn round_fn(&mut self, args: &[Expr], mode: RoundMode) -> CellValue {
        if args.is_empty() {
            return CellValue::Error("#ERR!".into());
        }
        let v = self.eval_expr(&args[0]);
        if v.is_error() {
            return v;
        }
        let Some(n) = v.as_number() else {
            return CellValue::Error("#VALUE!".into());
        };
        let digits = if args.len() >= 2 {
            self.eval_expr(&args[1]).as_number().unwrap_or(0.0) as i32
        } else {
            0
        };
        let factor = 10f64.powi(digits);
        let scaled = n * factor;
        let rounded = match mode {
            RoundMode::Nearest => scaled.round(),
            RoundMode::Down => scaled.trunc(),
            RoundMode::Up => {
                if scaled >= 0.0 {
                    scaled.ceil()
                } else {
                    scaled.floor()
                }
            }
        };
        CellValue::Number(rounded / factor)
    }

    fn unary_text(&mut self, args: &[Expr], f: impl Fn(&str) -> String) -> CellValue {
        let Some(a) = args.first() else {
            return CellValue::Error("#ERR!".into());
        };
        let v = self.eval_expr(a);
        if v.is_error() {
            return v;
        }
        CellValue::Text(f(&v.to_plain_string()))
    }

    fn substr_fn(&mut self, args: &[Expr], from_left: bool) -> CellValue {
        if args.is_empty() {
            return CellValue::Error("#ERR!".into());
        }
        let s = self.eval_expr(&args[0]).to_plain_string();
        let n = if args.len() >= 2 {
            self.eval_expr(&args[1]).as_number().unwrap_or(1.0).max(0.0) as usize
        } else {
            1
        };
        let chars: Vec<char> = s.chars().collect();
        let take = n.min(chars.len());
        let slice: String = if from_left {
            chars[..take].iter().collect()
        } else {
            chars[chars.len() - take..].iter().collect()
        };
        CellValue::Text(slice)
    }

    fn mid_fn(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 3 {
            return CellValue::Error("#ERR!".into());
        }
        let s = self.eval_expr(&args[0]).to_plain_string();
        let start = self.eval_expr(&args[1]).as_number().unwrap_or(1.0).max(1.0) as usize - 1;
        let len = self.eval_expr(&args[2]).as_number().unwrap_or(0.0).max(0.0) as usize;
        let chars: Vec<char> = s.chars().collect();
        if start >= chars.len() {
            return CellValue::Text(String::new());
        }
        let end = (start + len).min(chars.len());
        CellValue::Text(chars[start..end].iter().collect())
    }

    /// SUMIF(range, criterion, [sum_range]).
    fn sumif(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 2 {
            return CellValue::Error("#ERR!".into());
        }
        let probe = self.collect_values(&args[0]);
        let crit = self.eval_expr(&args[1]);
        let sum_vals = if args.len() >= 3 {
            self.collect_values(&args[2])
        } else {
            probe.clone()
        };
        let mut total = 0.0;
        for (i, v) in probe.iter().enumerate() {
            if criterion_matches(v, &crit) {
                if let Some(n) = sum_vals.get(i).and_then(|x| x.as_number()) {
                    total += n;
                }
            }
        }
        CellValue::Number(total)
    }

    /// COUNTIF(range, criterion).
    fn countif(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 2 {
            return CellValue::Error("#ERR!".into());
        }
        let probe = self.collect_values(&args[0]);
        let crit = self.eval_expr(&args[1]);
        let n = probe.iter().filter(|v| criterion_matches(v, &crit)).count();
        CellValue::Number(n as f64)
    }
}

#[derive(Clone, Copy)]
enum RoundMode {
    Nearest,
    Down,
    Up,
}

/// Whether a probe value satisfies a COUNTIF/SUMIF criterion. Supports a
/// bare value (equality) and the string operators `">5"`, `"<=3"`,
/// `"<>x"`, `">=0"`.
fn criterion_matches(probe: &CellValue, crit: &CellValue) -> bool {
    if let CellValue::Text(s) = crit {
        let s = s.trim();
        for (op, len) in [("<=", 2), (">=", 2), ("<>", 2), ("<", 1), (">", 1), ("=", 1)] {
            if let Some(rest) = s.strip_prefix(op) {
                let _ = len;
                if let Ok(threshold) = rest.trim().parse::<f64>() {
                    let Some(p) = probe.as_number() else { return false };
                    return match op {
                        "<=" => p <= threshold,
                        ">=" => p >= threshold,
                        "<>" => p != threshold,
                        "<" => p < threshold,
                        ">" => p > threshold,
                        "=" => p == threshold,
                        _ => false,
                    };
                }
                // text comparison for <> / =
                let rest = rest.trim();
                return match op {
                    "<>" => probe.to_plain_string() != rest,
                    "=" => probe.to_plain_string() == rest,
                    _ => false,
                };
            }
        }
    }
    // Plain equality.
    match (probe.as_number(), crit.as_number()) {
        (Some(a), Some(b)) => a == b,
        _ => probe.to_plain_string() == crit.to_plain_string(),
    }
}

/// Parse a non-formula raw string into a literal value. Numbers parse as
/// numbers, `TRUE`/`FALSE` as booleans, everything else is text.
fn parse_literal(raw: &str) -> CellValue {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return CellValue::Empty;
    }
    if let Ok(n) = trimmed.parse::<f64>() {
        return CellValue::Number(n);
    }
    // Accept a leading currency/percent for convenience.
    if let Some(p) = trimmed.strip_suffix('%') {
        if let Ok(n) = p.trim().parse::<f64>() {
            return CellValue::Number(n / 100.0);
        }
    }
    match trimmed.to_uppercase().as_str() {
        "TRUE" => CellValue::Bool(true),
        "FALSE" => CellValue::Bool(false),
        _ => CellValue::Text(raw.to_string()),
    }
}

/// Convenience: evaluate a whole sheet's raw inputs into a value map.
pub fn recalculate(raw: &HashMap<Addr, String>) -> HashMap<Addr, CellValue> {
    Engine::new(raw).evaluate_all()
}

// ---------------------------------------------------------------------------
// Format propagation
// ---------------------------------------------------------------------------

/// The unit a sub-expression carries while propagating formats. We track
/// the *category* (and the specific currency, when it's currency) rather
/// than a full `CellFormat`, because propagation cares about the unit, not
/// the exact rendering style.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    /// A bare scalar — `5`, `=2+2`, a General/Number cell. Multiplying a
    /// scalar by a currency yields that currency (`5 * $100 = $500`).
    Scalar,
    /// Money in a specific currency.
    Currency(crate::model::Currency),
    /// A percentage value.
    Percent,
    /// A date serial.
    Date(crate::model::DateStyle),
    /// A time/duration serial.
    Time(crate::model::TimeStyle),
    /// Conflicting units (e.g. USD + EUR) — render plain.
    Mixed,
}

/// Infer the display format a computed cell should adopt from the formats
/// of the cells its formula references. Returns `None` when nothing useful
/// can be inferred (the caller then leaves the cell's own format, defaulting
/// to General).
///
/// Rules, mirroring Excel/Sheets behaviour:
///   * `=5*B2` where B2 is currency → that currency (a scalar times money
///     stays money). Likewise `B2/2`, `B2+B3`, `-B2`.
///   * currency `+`/`-` currency of the **same** currency → that currency;
///     **different** currencies → plain number (Mixed).
///   * percent through `+`/`-`/scalar `*` → percent.
///   * date `-` date → a duration (Number); date `+` scalar → date.
///   * SUM/AVERAGE/MIN/MAX/etc. over a uniformly-formatted range inherit
///     that format.
///   * anything ambiguous → `None` (plain).
pub fn infer_result_format(
    body: &str,
    formats: &HashMap<Addr, CellFormat>,
) -> Option<CellFormat> {
    let expr = parse(body).ok()?;
    let unit = unit_of(&expr, formats);
    unit_to_format(unit)
}

fn unit_to_format(unit: Unit) -> Option<CellFormat> {
    match unit {
        Unit::Scalar | Unit::Mixed => None,
        Unit::Currency(c) => Some(CellFormat::Currency(c)),
        Unit::Percent => Some(CellFormat::Percent),
        Unit::Date(s) => Some(CellFormat::Date(s)),
        Unit::Time(s) => Some(CellFormat::Time(s)),
    }
}

/// The unit a cell's stored format implies.
fn unit_of_format(fmt: CellFormat) -> Unit {
    match fmt {
        CellFormat::Currency(c) => Unit::Currency(c),
        CellFormat::Percent => Unit::Percent,
        CellFormat::Date(s) => Unit::Date(s),
        CellFormat::Time(s) => Unit::Time(s),
        // General/Number/Boolean/Text all behave as scalars for propagation.
        _ => Unit::Scalar,
    }
}

/// Recursively determine an expression's unit.
fn unit_of(expr: &Expr, formats: &HashMap<Addr, CellFormat>) -> Unit {
    match expr {
        Expr::Num(_) | Expr::Bool(_) | Expr::Str(_) => Unit::Scalar,
        Expr::Ref(addr) => formats.get(addr).map(|f| unit_of_format(*f)).unwrap_or(Unit::Scalar),
        Expr::Range(_, _) => Unit::Scalar,
        Expr::Unary(_, inner) => unit_of(inner, formats),
        Expr::Binary(op, l, r) => {
            let lu = unit_of(l, formats);
            let ru = unit_of(r, formats);
            combine(*op, lu, ru)
        }
        Expr::Call(name, args) => unit_of_call(name, args, formats),
    }
}

/// Combine two operand units under a binary operator.
fn combine(op: BinOp, l: Unit, r: Unit) -> Unit {
    use BinOp::*;
    // Comparisons and concat yield bare scalars/booleans/text.
    if matches!(op, Eq | Ne | Lt | Gt | Le | Ge | Concat) {
        return Unit::Scalar;
    }
    match op {
        // Division of two equal units is a ratio → unitless (`$/$`, `%/%`).
        Div if l == r && l != Unit::Scalar => Unit::Scalar,
        // Multiplication/division by a scalar keeps the unit; any other
        // pairing of two real units is ambiguous (Mixed).
        Mul | Div | Pow => match (l, r) {
            (Unit::Scalar, u) | (u, Unit::Scalar) => u,
            _ => Unit::Mixed,
        },
        // Addition/subtraction: same unit stays; date−date becomes a
        // duration (treated as Number → Scalar); unit ± scalar keeps unit.
        Add | Sub => match (l, r) {
            (a, b) if a == b => {
                // date − date is a span of days, not a date.
                if matches!(op, Sub) && matches!(a, Unit::Date(_)) {
                    Unit::Scalar
                } else {
                    a
                }
            }
            (Unit::Scalar, u) | (u, Unit::Scalar) => u,
            _ => Unit::Mixed,
        },
        _ => Unit::Scalar,
    }
}

/// The unit a function call produces. Aggregates over a range inherit the
/// range's uniform unit; ROUND/ABS/etc. inherit their first argument.
fn unit_of_call(name: &str, args: &[Expr], formats: &HashMap<Addr, CellFormat>) -> Unit {
    match name {
        // Sum-family + selection functions preserve a uniform unit.
        "SUM" | "AVERAGE" | "MIN" | "MAX" | "MEDIAN" | "PRODUCT" | "ROUND" | "ROUNDUP"
        | "ROUNDDOWN" | "ABS" | "FLOOR" | "CEILING" | "INT" => {
            let mut acc: Option<Unit> = None;
            for a in args {
                for u in arg_units(a, formats) {
                    acc = Some(match acc {
                        None => u,
                        Some(prev) if prev == u => prev,
                        Some(Unit::Scalar) => u,
                        Some(prev) if u == Unit::Scalar => prev,
                        Some(_) => Unit::Mixed,
                    });
                }
            }
            acc.unwrap_or(Unit::Scalar)
        }
        // Counting / text / logical functions are unitless.
        _ => Unit::Scalar,
    }
}

/// Units contributed by a single argument — a range contributes the unit of
/// each cell it spans.
fn arg_units(arg: &Expr, formats: &HashMap<Addr, CellFormat>) -> Vec<Unit> {
    match arg {
        Expr::Range(a, b) => {
            let (c0, c1) = (a.0.min(b.0), a.0.max(b.0));
            let (r0, r1) = (a.1.min(b.1), a.1.max(b.1));
            let mut out = Vec::new();
            for r in r0..=r1 {
                for c in c0..=c1 {
                    out.push(formats.get(&(c, r)).map(|f| unit_of_format(*f)).unwrap_or(Unit::Scalar));
                }
            }
            out
        }
        _ => vec![unit_of(arg, formats)],
    }
}

/// True when a category should be allowed to propagate at all (kept for
/// callers that want to gate by category without importing `Unit`).
pub fn category_propagates(cat: FormatCategory) -> bool {
    matches!(
        cat,
        FormatCategory::Currency
            | FormatCategory::Percent
            | FormatCategory::Date
            | FormatCategory::Time
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cells: &[(&str, &str)]) -> HashMap<Addr, CellValue> {
        let mut raw = HashMap::new();
        for (a, v) in cells {
            raw.insert(a1_to_addr(a).unwrap(), v.to_string());
        }
        recalculate(&raw)
    }

    fn val(map: &HashMap<Addr, CellValue>, a: &str) -> CellValue {
        map.get(&a1_to_addr(a).unwrap()).cloned().unwrap_or(CellValue::Empty)
    }

    #[test]
    fn arithmetic_and_precedence() {
        let m = run(&[("A1", "=2+3*4"), ("A2", "=(2+3)*4"), ("A3", "=2^3^2")]);
        assert_eq!(val(&m, "A1"), CellValue::Number(14.0));
        assert_eq!(val(&m, "A2"), CellValue::Number(20.0));
        // right-assoc power: 2^(3^2) = 512
        assert_eq!(val(&m, "A3"), CellValue::Number(512.0));
    }

    #[test]
    fn cell_refs_and_sum_range() {
        let m = run(&[
            ("A1", "10"),
            ("A2", "20"),
            ("A3", "30"),
            ("B1", "=SUM(A1:A3)"),
            ("B2", "=AVERAGE(A1:A3)"),
            ("B3", "=A1+A2"),
        ]);
        assert_eq!(val(&m, "B1"), CellValue::Number(60.0));
        assert_eq!(val(&m, "B2"), CellValue::Number(20.0));
        assert_eq!(val(&m, "B3"), CellValue::Number(30.0));
    }

    #[test]
    fn if_and_comparisons() {
        let m = run(&[
            ("A1", "5"),
            ("B1", "=IF(A1>3,\"big\",\"small\")"),
            ("C1", "=IF(A1>10,1,0)"),
        ]);
        assert_eq!(val(&m, "B1"), CellValue::Text("big".into()));
        assert_eq!(val(&m, "C1"), CellValue::Number(0.0));
    }

    #[test]
    fn div_zero_and_cycle() {
        let m = run(&[("A1", "=1/0"), ("B1", "=B2"), ("B2", "=B1")]);
        assert_eq!(val(&m, "A1"), CellValue::Error("#DIV/0!".into()));
        assert!(val(&m, "B1").is_error());
    }

    #[test]
    fn text_functions() {
        let m = run(&[
            ("A1", "=UPPER(\"hi\")"),
            ("A2", "=LEFT(\"hello\",2)"),
            ("A3", "=CONCAT(\"a\",\"b\",\"c\")"),
            ("A4", "=LEN(\"abcd\")"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Text("HI".into()));
        assert_eq!(val(&m, "A2"), CellValue::Text("he".into()));
        assert_eq!(val(&m, "A3"), CellValue::Text("abc".into()));
        assert_eq!(val(&m, "A4"), CellValue::Number(4.0));
    }

    #[test]
    fn countif_sumif() {
        let m = run(&[
            ("A1", "5"),
            ("A2", "15"),
            ("A3", "25"),
            ("B1", "=COUNTIF(A1:A3,\">10\")"),
            ("B2", "=SUMIF(A1:A3,\">=15\")"),
        ]);
        assert_eq!(val(&m, "B1"), CellValue::Number(2.0));
        assert_eq!(val(&m, "B2"), CellValue::Number(40.0));
    }

    // --- format propagation -------------------------------------------------

    use crate::model::{CellFormat, Currency, DateStyle};

    fn fmts(pairs: &[(&str, CellFormat)]) -> HashMap<Addr, CellFormat> {
        pairs
            .iter()
            .map(|(a, f)| (a1_to_addr(a).unwrap(), *f))
            .collect()
    }

    #[test]
    fn scalar_times_currency_is_currency() {
        // =5*B2 where B2 is $100 must render as currency.
        let f = fmts(&[("B2", CellFormat::Currency(Currency::USD))]);
        assert_eq!(
            infer_result_format("5*B2", &f),
            Some(CellFormat::Currency(Currency::USD))
        );
        // order doesn't matter
        assert_eq!(
            infer_result_format("B2*5", &f),
            Some(CellFormat::Currency(Currency::USD))
        );
        // division by a scalar keeps the currency
        assert_eq!(
            infer_result_format("B2/2", &f),
            Some(CellFormat::Currency(Currency::USD))
        );
    }

    #[test]
    fn currency_plus_same_currency_propagates() {
        let f = fmts(&[
            ("A1", CellFormat::Currency(Currency::EUR)),
            ("A2", CellFormat::Currency(Currency::EUR)),
        ]);
        assert_eq!(
            infer_result_format("A1+A2", &f),
            Some(CellFormat::Currency(Currency::EUR))
        );
    }

    #[test]
    fn mixed_currencies_fall_back_to_plain() {
        let f = fmts(&[
            ("A1", CellFormat::Currency(Currency::USD)),
            ("A2", CellFormat::Currency(Currency::EUR)),
        ]);
        // USD + EUR is unclear → plain number (None).
        assert_eq!(infer_result_format("A1+A2", &f), None);
        // USD * EUR is also unclear.
        assert_eq!(infer_result_format("A1*A2", &f), None);
    }

    #[test]
    fn percent_arithmetic_propagates() {
        let f = fmts(&[("A1", CellFormat::Percent), ("A2", CellFormat::Percent)]);
        assert_eq!(infer_result_format("A1+A2", &f), Some(CellFormat::Percent));
        assert_eq!(infer_result_format("A1*2", &f), Some(CellFormat::Percent));
    }

    #[test]
    fn sum_over_currency_range_inherits_currency() {
        let f = fmts(&[
            ("B2", CellFormat::Currency(Currency::GBP)),
            ("B3", CellFormat::Currency(Currency::GBP)),
            ("B4", CellFormat::Currency(Currency::GBP)),
        ]);
        assert_eq!(
            infer_result_format("SUM(B2:B4)", &f),
            Some(CellFormat::Currency(Currency::GBP))
        );
        assert_eq!(
            infer_result_format("AVERAGE(B2:B4)", &f),
            Some(CellFormat::Currency(Currency::GBP))
        );
    }

    #[test]
    fn date_minus_date_is_a_duration_not_a_date() {
        let f = fmts(&[
            ("A1", CellFormat::Date(DateStyle::Iso)),
            ("A2", CellFormat::Date(DateStyle::Iso)),
        ]);
        // Difference of two dates is a number of days → plain (None).
        assert_eq!(infer_result_format("A1-A2", &f), None);
        // A date plus a scalar stays a date.
        assert_eq!(
            infer_result_format("A1+7", &f),
            Some(CellFormat::Date(DateStyle::Iso))
        );
    }

    #[test]
    fn plain_arithmetic_has_no_format() {
        let f = fmts(&[]);
        assert_eq!(infer_result_format("2+3*4", &f), None);
    }

    #[test]
    fn comparison_and_text_are_unitless() {
        let f = fmts(&[("A1", CellFormat::Currency(Currency::USD))]);
        assert_eq!(infer_result_format("A1>100", &f), None);
        assert_eq!(infer_result_format("A1&\"x\"", &f), None);
    }

    #[test]
    fn growth_ratio_is_plain_even_with_currency_operands() {
        // (E2-B2)/B2 where E2,B2 are currency → a ratio, which is unitless.
        let f = fmts(&[
            ("E2", CellFormat::Currency(Currency::USD)),
            ("B2", CellFormat::Currency(Currency::USD)),
        ]);
        // (currency - currency) = currency; currency / currency = Mixed→None.
        assert_eq!(infer_result_format("(E2-B2)/B2", &f), None);
    }
}
