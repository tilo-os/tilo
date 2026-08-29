//! Formula engine: tokenizer -> recursive-descent parser -> evaluator.
//!
//! Supports a practical subset of the spreadsheet formula language:
//!   * literals: numbers, `"strings"`, `TRUE` / `FALSE`
//!   * cell refs `A1`, absolute `$A$1`, and ranges `A1:B3`
//!   * arithmetic `+ - * / ^` and unary `-`/`+`
//!   * text concatenation `&`
//!   * comparisons `= <> < > <= >=`
//!   * math: SUM, PRODUCT, ABS, SQRT, POWER, MOD, INT, TRUNC, ROUND,
//!     ROUNDDOWN, ROUNDUP, FLOOR, CEILING, SIGN, EXP, LN, LOG, LOG10, PI
//!   * stats: AVERAGE, MEDIAN, MIN, MAX, COUNT, COUNTA, COUNTBLANK,
//!     COUNTIF, COUNTIFS, SUMIF, SUMIFS, AVERAGEIF, AVERAGEIFS
//!   * logical: IF, IFS, IFERROR, IFNA, AND, OR, NOT, XOR, SWITCH,
//!     TRUE, FALSE
//!   * text: LEN, UPPER, LOWER, PROPER, TRIM, LEFT, RIGHT, MID, REPLACE,
//!     SUBSTITUTE, FIND, SEARCH, REPT, CONCAT/CONCATENATE, TEXTJOIN, TEXT,
//!     VALUE, EXACT, CHAR, CODE
//!   * lookup: VLOOKUP, HLOOKUP, INDEX, MATCH, CHOOSE
//!   * date/time: TODAY, NOW, DATE, TIME, YEAR, MONTH, DAY, HOUR, MINUTE,
//!     SECOND, WEEKDAY
//!
//! Evaluation is memoised with on-the-fly cycle detection so a sheet with
//! `A1 = B1 + 1`, `B1 = A1` resolves to `#CYCLE!` rather than recursing
//! forever. References that point at unparseable cells yield `#REF!`;
//! division by zero yields `#DIV/0!`; type mismatches yield `#VALUE!`.

use std::collections::{HashMap, HashSet};

use crate::model::{
    Addr, CellFormat, CellValue, FormatCategory, FormulaError, a1_to_addr, addr_to_a1,
};

// ---------------------------------------------------------------------------
// Reference extraction (for editor highlighting + click-to-insert)
// ---------------------------------------------------------------------------

/// A cell or range reference located inside a formula body, used to tint the
/// reference where it appears in the formula text and to outline the cells it
/// points at on the grid — the coloured-reference cue Excel/Calc show while a
/// formula is being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct RefSpan {
    /// Char index into the body (the text after `=`) where the reference
    /// token starts.
    pub start: usize,
    /// Char index one past the reference token's end.
    pub end: usize,
    /// Top-left cell of the referenced rectangle (equals `br` for a single
    /// cell), normalized so `tl <= br`.
    pub tl: Addr,
    /// Bottom-right cell of the referenced rectangle.
    pub br: Addr,
    /// Stable key used for colour assignment — identical references (ignoring
    /// `$` anchors and letter case) share a colour.
    pub key: String,
}

/// Scan a formula body (the text after the leading `=`) for cell and range
/// references, returning each one's character span and target rectangle.
///
/// String literals are skipped, and an identifier immediately followed by `(`
/// is treated as a function name rather than a reference (so `SUM` in
/// `SUM(A1:A3)` is not tinted, but `A1:A3` is). Numeric exponents like `1e3`
/// are never mistaken for references.
pub fn references(body: &str) -> Vec<RefSpan> {
    let chars: Vec<char> = body.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            if i < chars.len() {
                i += 1; // closing quote
            }
            continue;
        }
        if c == '$' || c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i] == '$' || chars[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            // A run right after a digit/dot is an exponent (`1e3`), not a ref.
            let prev_numish =
                start > 0 && (chars[start - 1].is_ascii_digit() || chars[start - 1] == '.');
            // An identifier followed by `(` is a function name, not a ref.
            let mut k = i;
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            let is_call = k < chars.len() && chars[k] == '(';
            if prev_numish || is_call {
                continue;
            }
            let token: String = chars[start..i].iter().collect();
            let Some(a0) = a1_to_addr(&token) else {
                continue; // TRUE / FALSE / function names / bare words
            };
            // A `:` (optionally spaced) followed by another reference makes a
            // range that spans from `token` to the second reference.
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && chars[j] == ':' {
                let mut m = j + 1;
                while m < chars.len() && chars[m].is_whitespace() {
                    m += 1;
                }
                let tstart = m;
                while m < chars.len() && (chars[m] == '$' || chars[m].is_ascii_alphanumeric()) {
                    m += 1;
                }
                let tok2: String = chars[tstart..m].iter().collect();
                if let Some(a1v) = a1_to_addr(&tok2) {
                    let tl = (a0.0.min(a1v.0), a0.1.min(a1v.1));
                    let br = (a0.0.max(a1v.0), a0.1.max(a1v.1));
                    let key = format!("{}:{}", addr_to_a1(tl.0, tl.1), addr_to_a1(br.0, br.1));
                    out.push(RefSpan {
                        start,
                        end: m,
                        tl,
                        br,
                        key,
                    });
                    i = m;
                    continue;
                }
            }
            let key = addr_to_a1(a0.0, a0.1);
            out.push(RefSpan {
                start,
                end: i,
                tl: a0,
                br: a0,
                key,
            });
            continue;
        }
        i += 1;
    }
    out
}

/// Hand each reference span a colour slot so identical references share a
/// slot; slots are assigned in first-appearance order. Returns a vector
/// parallel to `spans`. The view layer maps a slot to a concrete colour.
pub fn ref_color_slots(spans: &[RefSpan]) -> Vec<usize> {
    let mut map: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::with_capacity(spans.len());
    for s in spans {
        let next = map.len();
        out.push(*map.entry(s.key.as_str()).or_insert(next));
    }
    out
}

// ---------------------------------------------------------------------------
// Reference adjustment (copy/paste + fill relativisation)
// ---------------------------------------------------------------------------

/// Rewrite a formula so its **relative** cell references shift by `(dcol,
/// drow)` — the adjustment every spreadsheet applies when a formula is copied
/// or filled to a new location. This is the formula layer's single source of
/// truth for relativisation; the copy/paste and fill paths both route through
/// it.
///
/// The rules match Excel/Sheets exactly:
///   * a bare `A1` shifts on both axes (`=A1` copied one row down → `=A2`);
///   * `$A$1` is fully anchored and never moves;
///   * `$A1` locks the column, so only the row shifts;
///   * `A$1` locks the row, so only the column shifts;
///   * a range shifts each endpoint independently (`A1:B3` → `B2:C4`);
///   * string literals, function names, and numeric literals (including
///     scientific notation like `1e3`) are never mistaken for references.
///
/// Non-formula input (no leading `=`) and a zero offset are returned
/// unchanged. A relative component never underflows past the grid origin
/// (column `A` / row `1`).
pub fn offset_refs(raw: &str, dcol: i64, drow: i64) -> String {
    let Some(body) = raw.strip_prefix('=') else {
        return raw.to_string();
    };
    if dcol == 0 && drow == 0 {
        return raw.to_string();
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(raw.len() + 4);
    out.push('=');
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            // Copy a string literal verbatim, including the closing quote.
            out.push(c);
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                let done = chars[i] == '"';
                i += 1;
                if done {
                    break;
                }
            }
            continue;
        }
        if c == '$' || c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i] == '$' || chars[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            // A run right after a digit/dot is an exponent (`1e3`), not a ref.
            let prev_numish =
                start > 0 && (chars[start - 1].is_ascii_digit() || chars[start - 1] == '.');
            // An identifier immediately followed by `(` is a function name.
            let mut k = i;
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            let is_call = k < chars.len() && chars[k] == '(';
            if !prev_numish && !is_call
                && let Some(shifted) = offset_ref_token(&token, dcol, drow) {
                    out.push_str(&shifted);
                    continue;
                }
            out.push_str(&token);
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Shift one `A1`/`$A$1`-shaped token by `(dcol, drow)`, preserving each `$`
/// anchor. Returns `None` when the token is not a well-formed reference
/// (leaving function names, booleans, and bare words untouched).
fn offset_ref_token(tok: &str, dcol: i64, drow: i64) -> Option<String> {
    let bytes = tok.as_bytes();
    let mut i = 0;
    let col_abs = i < bytes.len() && bytes[i] == b'$';
    if col_abs {
        i += 1;
    }
    let ls = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == ls {
        return None;
    }
    let letters = &tok[ls..i];
    let row_abs = i < bytes.len() && bytes[i] == b'$';
    if row_abs {
        i += 1;
    }
    let ds = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == ds || i != bytes.len() {
        return None;
    }
    let col = i64::from(crate::model::letters_to_col(letters)?);
    let row: i64 = tok[ds..i].parse().ok()?; // 1-based
    let new_col = if col_abs { col } else { (col + dcol).max(0) };
    let new_row = if row_abs { row } else { (row + drow).max(1) };
    let mut s = String::new();
    if col_abs {
        s.push('$');
    }
    s.push_str(&crate::model::col_to_letters(new_col as u32));
    if row_abs {
        s.push('$');
    }
    s.push_str(&new_row.to_string());
    Some(s)
}

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

fn tokenize(input: &str) -> Result<Vec<Token>, FormulaError> {
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
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
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
                let n = s.parse::<f64>().map_err(|_| FormulaError::Error)?;
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
                    return Err(FormulaError::Error);
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
            '+' | '-' | '*' | '/' | '^' | '&' | '%' | '(' | ')' | ',' | ':' => {
                out.push(Token::Op(c));
                i += 1;
            }
            _ => return Err(FormulaError::Error),
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

    fn parse(&mut self) -> Result<Expr, FormulaError> {
        let e = self.parse_comparison()?;
        if self.pos != self.tokens.len() {
            return Err(FormulaError::Error);
        }
        Ok(e)
    }

    fn parse_comparison(&mut self) -> Result<Expr, FormulaError> {
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

    fn parse_concat(&mut self) -> Result<Expr, FormulaError> {
        let mut left = self.parse_add()?;
        while matches!(self.peek(), Some(Token::Op('&'))) {
            self.next();
            let right = self.parse_add()?;
            left = Expr::Binary(BinOp::Concat, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Expr, FormulaError> {
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

    fn parse_mul(&mut self) -> Result<Expr, FormulaError> {
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

    fn parse_unary(&mut self) -> Result<Expr, FormulaError> {
        if let Some(Token::Op(c @ ('-' | '+'))) = self.peek().cloned() {
            self.next();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary(c, Box::new(e)));
        }
        self.parse_power()
    }

    fn parse_power(&mut self) -> Result<Expr, FormulaError> {
        let base = self.parse_postfix()?;
        if matches!(self.peek(), Some(Token::Op('^'))) {
            self.next();
            // Right-associative exponentiation.
            let exp = self.parse_unary()?;
            return Ok(Expr::Binary(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn parse_postfix(&mut self) -> Result<Expr, FormulaError> {
        let mut e = self.parse_primary()?;
        // Percent literal suffix: `50%` -> 0.5.
        while matches!(self.peek(), Some(Token::Op('%'))) {
            self.next();
            e = Expr::Binary(BinOp::Div, Box::new(e), Box::new(Expr::Num(100.0)));
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, FormulaError> {
        match self.next() {
            Some(Token::Num(n)) => Ok(Expr::Num(n)),
            Some(Token::Str(s)) => Ok(Expr::Str(s)),
            Some(Token::Op('(')) => {
                let e = self.parse_comparison()?;
                match self.next() {
                    Some(Token::Op(')')) => Ok(e),
                    _ => Err(FormulaError::Error),
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
                        _ => return Err(FormulaError::Error),
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
                        _ => return Err(FormulaError::Ref),
                    };
                    let a = a1_to_addr(&id).ok_or(FormulaError::Ref)?;
                    let b = a1_to_addr(&end).ok_or(FormulaError::Ref)?;
                    return Ok(Expr::Range(a, b));
                }
                // Single cell reference; an identifier that isn't a valid A1
                // reference is an unknown name.
                let addr = a1_to_addr(&id).ok_or(FormulaError::Name)?;
                Ok(Expr::Ref(addr))
            }
            _ => Err(FormulaError::Error),
        }
    }
}

/// Parse a formula body (without the leading `=`) into an `Expr`. The
/// returned tree can be evaluated repeatedly against changing cell data.
fn parse(formula: &str) -> Result<Expr, FormulaError> {
    let tokens = tokenize(formula)?;
    if tokens.is_empty() {
        return Err(FormulaError::Error);
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
            return CellValue::Error(FormulaError::Cycle);
        }
        let raw = match self.raw.get(&addr) {
            Some(r) if !r.is_empty() => r.clone(),
            _ => return CellValue::Empty,
        };

        let value = if let Some(body) = raw.strip_prefix('=') {
            self.visiting.insert(addr);
            let v = match parse(body) {
                Ok(expr) => self.eval_expr(&expr),
                Err(e) => CellValue::Error(e),
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
            Expr::Range(_, _) => CellValue::Error(FormulaError::Value),
            Expr::Unary(op, inner) => {
                let v = self.eval_expr(inner);
                if v.is_error() {
                    return v;
                }
                match v.as_number() {
                    Some(n) => CellValue::Number(if *op == '-' { -n } else { n }),
                    None => CellValue::Error(FormulaError::Value),
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
            return CellValue::Text(format!("{}{}", lv.to_plain_string(), rv.to_plain_string()));
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
            _ => return CellValue::Error(FormulaError::Value),
        };
        let result = match op {
            BinOp::Add => a + b,
            BinOp::Sub => a - b,
            BinOp::Mul => a * b,
            BinOp::Div => {
                if b == 0.0 {
                    return CellValue::Error(FormulaError::Div0);
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
            return CellValue::Error(FormulaError::Value);
        };
        use std::cmp::Ordering::{Equal, Less, Greater};
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
                Ok(_) => CellValue::Error(FormulaError::Div0),
                Err(e) => e,
            },
            "MEDIAN" => match self.numeric_args(args) {
                Ok(mut ns) if !ns.is_empty() => {
                    ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let mid = ns.len() / 2;
                    let m = if ns.len() % 2 == 0 {
                        f64::midpoint(ns[mid - 1], ns[mid])
                    } else {
                        ns[mid]
                    };
                    CellValue::Number(m)
                }
                Ok(_) => CellValue::Error(FormulaError::Num),
                Err(e) => e,
            },
            "MIN" => match self.numeric_args(args) {
                Ok(ns) => {
                    if ns.is_empty() {
                        CellValue::Number(0.0)
                    } else {
                        CellValue::Number(ns.iter().copied().fold(f64::INFINITY, f64::min))
                    }
                }
                Err(e) => e,
            },
            "MAX" => match self.numeric_args(args) {
                Ok(ns) => {
                    if ns.is_empty() {
                        CellValue::Number(0.0)
                    } else {
                        CellValue::Number(ns.iter().copied().fold(f64::NEG_INFINITY, f64::max))
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
                CellValue::Number(f64::from(n))
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
                CellValue::Number(f64::from(n))
            }
            "IF" => {
                if args.len() < 2 {
                    return CellValue::Error(FormulaError::Error);
                }
                let cond = self.eval_expr(&args[0]);
                if cond.is_error() {
                    return cond;
                }
                if cond_truthy(&cond) {
                    self.eval_expr(&args[1])
                } else if args.len() >= 3 {
                    self.eval_expr(&args[2])
                } else {
                    CellValue::Bool(false)
                }
            }
            "IFS" => {
                let mut i = 0;
                while i + 1 < args.len() {
                    let cond = self.eval_expr(&args[i]);
                    if cond.is_error() {
                        return cond;
                    }
                    if cond_truthy(&cond) {
                        return self.eval_expr(&args[i + 1]);
                    }
                    i += 2;
                }
                CellValue::Error(FormulaError::Na)
            }
            "IFERROR" => {
                if args.len() < 2 {
                    return CellValue::Error(FormulaError::Error);
                }
                let v = self.eval_expr(&args[0]);
                if v.is_error() {
                    self.eval_expr(&args[1])
                } else {
                    v
                }
            }
            "IFNA" => {
                if args.len() < 2 {
                    return CellValue::Error(FormulaError::Error);
                }
                let v = self.eval_expr(&args[0]);
                if matches!(&v, CellValue::Error(FormulaError::Na)) {
                    self.eval_expr(&args[1])
                } else {
                    v
                }
            }
            "SWITCH" => {
                if args.is_empty() {
                    return CellValue::Error(FormulaError::Error);
                }
                let target = self.eval_expr(&args[0]);
                let mut i = 1;
                while i + 1 < args.len() {
                    let case = self.eval_expr(&args[i]);
                    if values_equal(&case, &target) {
                        return self.eval_expr(&args[i + 1]);
                    }
                    i += 2;
                }
                // A lone trailing argument is the default.
                if i < args.len() {
                    self.eval_expr(&args[i])
                } else {
                    CellValue::Error(FormulaError::Na)
                }
            }
            "TRUE" => CellValue::Bool(true),
            "FALSE" => CellValue::Bool(false),
            "XOR" => match self.flatten_bools(args) {
                Ok(bs) => CellValue::Bool(bs.iter().filter(|b| **b).count() % 2 == 1),
                Err(e) => e,
            },
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
                    return CellValue::Error(FormulaError::Error);
                }
                let v = self.eval_expr(&args[0]);
                match v.as_number() {
                    Some(n) => CellValue::Bool(n == 0.0),
                    None => CellValue::Error(FormulaError::Value),
                }
            }
            "ABS" => self.unary_num(args, f64::abs),
            "SQRT" => self.unary_num(args, f64::sqrt),
            "INT" => self.unary_num(args, f64::floor),
            "SIGN" => self.unary_num(args, f64::signum),
            "EXP" => self.unary_num(args, f64::exp),
            "LN" => self.unary_num(args, f64::ln),
            "LOG10" => self.unary_num(args, f64::log10),
            "LOG" => {
                let v = self.eval_expr(args.first().unwrap_or(&Expr::Num(0.0)));
                if v.is_error() {
                    return v;
                }
                let Some(n) = v.as_number() else {
                    return CellValue::Error(FormulaError::Value);
                };
                let base = args
                    .get(1)
                    .and_then(|a| self.eval_expr(a).as_number())
                    .unwrap_or(10.0);
                CellValue::Number(n.log(base))
            }
            "PI" => CellValue::Number(std::f64::consts::PI),
            "ROUND" => self.round_fn(args, RoundMode::Nearest),
            "ROUNDDOWN" | "TRUNC" => self.round_fn(args, RoundMode::Down),
            "ROUNDUP" => self.round_fn(args, RoundMode::Up),
            "MOD" => self.binary_num(args, |a, b| {
                if b == 0.0 {
                    None
                } else {
                    Some(a - b * (a / b).floor())
                }
            }),
            "POWER" => self.binary_num(args, |a, b| Some(a.powf(b))),
            "FLOOR" => self.unary_num(args, f64::floor),
            "CEILING" => self.unary_num(args, f64::ceil),
            "LEN" => {
                let v = self.eval_expr(args.first().unwrap_or(&Expr::Str(String::new())));
                CellValue::Number(v.to_plain_string().chars().count() as f64)
            }
            "UPPER" => self.unary_text(args, str::to_uppercase),
            "LOWER" => self.unary_text(args, str::to_lowercase),
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
            "SUBSTITUTE" => self.substitute(args),
            "REPLACE" => self.replace_fn(args),
            "FIND" => self.find_fn(args, true),
            "SEARCH" => self.find_fn(args, false),
            "PROPER" => self.unary_text(args, proper_case),
            "REPT" => {
                let s = self
                    .eval_expr(args.first().unwrap_or(&Expr::Str(String::new())))
                    .to_plain_string();
                let n = args
                    .get(1)
                    .and_then(|a| self.eval_expr(a).as_number())
                    .unwrap_or(0.0)
                    .max(0.0) as usize;
                CellValue::Text(s.repeat(n.min(10_000)))
            }
            "VALUE" => {
                let s = self
                    .eval_expr(args.first().unwrap_or(&Expr::Str(String::new())))
                    .to_plain_string();
                let cleaned: String = s
                    .chars()
                    .filter(|c| !matches!(c, ',' | '$' | ' '))
                    .collect();
                let core = cleaned.trim_end_matches('%');
                match core.parse::<f64>() {
                    Ok(n) => CellValue::Number(if cleaned.ends_with('%') { n / 100.0 } else { n }),
                    Err(_) => CellValue::Error(FormulaError::Value),
                }
            }
            "EXACT" => {
                if args.len() < 2 {
                    return CellValue::Error(FormulaError::Error);
                }
                let a = self.eval_expr(&args[0]).to_plain_string();
                let b = self.eval_expr(&args[1]).to_plain_string();
                CellValue::Bool(a == b)
            }
            "CHAR" => {
                let n = self
                    .eval_expr(args.first().unwrap_or(&Expr::Num(0.0)))
                    .as_number()
                    .unwrap_or(0.0) as u32;
                match char::from_u32(n) {
                    Some(c) => CellValue::Text(c.to_string()),
                    None => CellValue::Error(FormulaError::Value),
                }
            }
            "CODE" => {
                let s = self
                    .eval_expr(args.first().unwrap_or(&Expr::Str(String::new())))
                    .to_plain_string();
                match s.chars().next() {
                    Some(c) => CellValue::Number(f64::from(c as u32)),
                    None => CellValue::Error(FormulaError::Value),
                }
            }
            "TEXTJOIN" => self.textjoin(args),
            "TEXT" => {
                if args.len() < 2 {
                    return CellValue::Error(FormulaError::Error);
                }
                let v = self.eval_expr(&args[0]);
                if v.is_error() {
                    return v;
                }
                let code = self.eval_expr(&args[1]).to_plain_string();
                match v.as_number() {
                    Some(n) => CellValue::Text(apply_text_code(n, &code)),
                    None => CellValue::Text(v.to_plain_string()),
                }
            }
            "SUMIF" => self.sumif(args),
            "COUNTIF" => self.countif(args),
            "AVERAGEIF" => self.averageif(args),
            "COUNTBLANK" => {
                let mut n = 0;
                for a in args {
                    for v in self.collect_values(a) {
                        if v.is_empty() {
                            n += 1;
                        }
                    }
                }
                CellValue::Number(f64::from(n))
            }
            "COUNTIFS" => match self.criteria_mask(args) {
                Ok(mask) => CellValue::Number(mask.iter().filter(|b| **b).count() as f64),
                Err(e) => e,
            },
            "SUMIFS" => {
                if args.is_empty() {
                    return CellValue::Error(FormulaError::Error);
                }
                let vals = self.collect_values(&args[0]);
                match self.criteria_mask(&args[1..]) {
                    Ok(mask) => {
                        let mut total = 0.0;
                        for (i, keep) in mask.iter().enumerate() {
                            if *keep
                                && let Some(n) = vals.get(i).and_then(super::model::CellValue::as_number) {
                                    total += n;
                                }
                        }
                        CellValue::Number(total)
                    }
                    Err(e) => e,
                }
            }
            "AVERAGEIFS" => {
                if args.is_empty() {
                    return CellValue::Error(FormulaError::Error);
                }
                let vals = self.collect_values(&args[0]);
                match self.criteria_mask(&args[1..]) {
                    Ok(mask) => {
                        let (mut sum, mut cnt) = (0.0, 0usize);
                        for (i, keep) in mask.iter().enumerate() {
                            if *keep
                                && let Some(n) = vals.get(i).and_then(super::model::CellValue::as_number) {
                                    sum += n;
                                    cnt += 1;
                                }
                        }
                        if cnt == 0 {
                            CellValue::Error(FormulaError::Div0)
                        } else {
                            CellValue::Number(sum / cnt as f64)
                        }
                    }
                    Err(e) => e,
                }
            }
            "VLOOKUP" => self.vlookup(args),
            "HLOOKUP" => self.hlookup(args),
            "INDEX" => self.index_fn(args),
            "MATCH" => self.match_fn(args),
            "CHOOSE" => {
                let idx = self
                    .eval_expr(args.first().unwrap_or(&Expr::Num(0.0)))
                    .as_number()
                    .unwrap_or(0.0) as i64;
                if idx >= 1 && (idx as usize) < args.len() {
                    self.eval_expr(&args[idx as usize])
                } else {
                    CellValue::Error(FormulaError::Value)
                }
            }
            "DATE" => {
                if args.len() < 3 {
                    return CellValue::Error(FormulaError::Error);
                }
                let y = self.eval_expr(&args[0]).as_number();
                let m = self.eval_expr(&args[1]).as_number();
                let d = self.eval_expr(&args[2]).as_number();
                match (y, m, d) {
                    (Some(y), Some(m), Some(d)) => {
                        // Normalize out-of-range months/days the way Excel does.
                        let m0 = m as i64 - 1;
                        let year = y as i64 + m0.div_euclid(12);
                        let month = m0.rem_euclid(12) as u32 + 1;
                        let serial =
                            crate::model::date_to_serial(year, month, 1) + (d as i64 - 1) as f64;
                        CellValue::Number(serial)
                    }
                    _ => CellValue::Error(FormulaError::Value),
                }
            }
            "TIME" => {
                if args.is_empty() {
                    return CellValue::Error(FormulaError::Error);
                }
                let h = self.eval_expr(&args[0]).as_number().unwrap_or(0.0);
                let mi = args
                    .get(1)
                    .and_then(|a| self.eval_expr(a).as_number())
                    .unwrap_or(0.0);
                let s = args
                    .get(2)
                    .and_then(|a| self.eval_expr(a).as_number())
                    .unwrap_or(0.0);
                CellValue::Number(((h * 3600.0 + mi * 60.0 + s) / 86400.0).rem_euclid(1.0))
            }
            "TODAY" => CellValue::Number(crate::model::now_serial().floor()),
            "NOW" => CellValue::Number(crate::model::now_serial()),
            "YEAR" => self.date_part(args, |(y, _, _)| y as f64),
            "MONTH" => self.date_part(args, |(_, m, _)| f64::from(m)),
            "DAY" => self.date_part(args, |(_, _, d)| f64::from(d)),
            "HOUR" => self.time_part(args, 0),
            "MINUTE" => self.time_part(args, 1),
            "SECOND" => self.time_part(args, 2),
            "WEEKDAY" => {
                let v = self.eval_expr(args.first().unwrap_or(&Expr::Num(0.0)));
                if v.is_error() {
                    return v;
                }
                let Some(n) = v.as_number() else {
                    return CellValue::Error(FormulaError::Value);
                };
                let wd = crate::model::weekday_from_serial(n); // 1=Sun..7=Sat
                let t = args
                    .get(1)
                    .and_then(|a| self.eval_expr(a).as_number())
                    .map_or(1, |x| x as i64);
                let out = match t {
                    2 => ((wd + 5) % 7) + 1, // Mon=1..Sun=7
                    3 => (wd + 5) % 7,       // Mon=0..Sun=6
                    _ => wd,                 // Sun=1..Sat=7
                };
                CellValue::Number(f64::from(out))
            }
            _ => CellValue::Error(FormulaError::Name),
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
            return CellValue::Error(FormulaError::Error);
        };
        let v = self.eval_expr(a);
        if v.is_error() {
            return v;
        }
        match v.as_number() {
            Some(n) => CellValue::Number(f(n)),
            None => CellValue::Error(FormulaError::Value),
        }
    }

    fn binary_num(&mut self, args: &[Expr], f: impl Fn(f64, f64) -> Option<f64>) -> CellValue {
        if args.len() != 2 {
            return CellValue::Error(FormulaError::Error);
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
                None => CellValue::Error(FormulaError::Div0),
            },
            _ => CellValue::Error(FormulaError::Value),
        }
    }

    fn round_fn(&mut self, args: &[Expr], mode: RoundMode) -> CellValue {
        if args.is_empty() {
            return CellValue::Error(FormulaError::Error);
        }
        let v = self.eval_expr(&args[0]);
        if v.is_error() {
            return v;
        }
        let Some(n) = v.as_number() else {
            return CellValue::Error(FormulaError::Value);
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
            return CellValue::Error(FormulaError::Error);
        };
        let v = self.eval_expr(a);
        if v.is_error() {
            return v;
        }
        CellValue::Text(f(&v.to_plain_string()))
    }

    fn substr_fn(&mut self, args: &[Expr], from_left: bool) -> CellValue {
        if args.is_empty() {
            return CellValue::Error(FormulaError::Error);
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
            return CellValue::Error(FormulaError::Error);
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

    /// SUMIF(range, criterion, [`sum_range`]).
    fn sumif(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 2 {
            return CellValue::Error(FormulaError::Error);
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
            if criterion_matches(v, &crit)
                && let Some(n) = sum_vals.get(i).and_then(super::model::CellValue::as_number) {
                    total += n;
                }
        }
        CellValue::Number(total)
    }

    /// COUNTIF(range, criterion).
    fn countif(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 2 {
            return CellValue::Error(FormulaError::Error);
        }
        let probe = self.collect_values(&args[0]);
        let crit = self.eval_expr(&args[1]);
        let n = probe.iter().filter(|v| criterion_matches(v, &crit)).count();
        CellValue::Number(n as f64)
    }

    /// AVERAGEIF(range, criterion, [`average_range`]).
    fn averageif(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 2 {
            return CellValue::Error(FormulaError::Error);
        }
        let probe = self.collect_values(&args[0]);
        let crit = self.eval_expr(&args[1]);
        let vals = if args.len() >= 3 {
            self.collect_values(&args[2])
        } else {
            probe.clone()
        };
        let (mut sum, mut cnt) = (0.0, 0usize);
        for (i, v) in probe.iter().enumerate() {
            if criterion_matches(v, &crit)
                && let Some(n) = vals.get(i).and_then(super::model::CellValue::as_number) {
                    sum += n;
                    cnt += 1;
                }
        }
        if cnt == 0 {
            CellValue::Error(FormulaError::Div0)
        } else {
            CellValue::Number(sum / cnt as f64)
        }
    }

    /// Build the per-cell keep mask for the `*IFS` family from a flat list of
    /// `range, criterion` pairs. Every criterion must hold for a position to be
    /// kept; ranges shorter than the first simply stop contributing.
    fn criteria_mask(&mut self, pairs: &[Expr]) -> Result<Vec<bool>, CellValue> {
        let mut mask: Option<Vec<bool>> = None;
        let mut i = 0;
        while i + 1 < pairs.len() {
            let probe = self.collect_values(&pairs[i]);
            let crit = self.eval_expr(&pairs[i + 1]);
            if crit.is_error() {
                return Err(crit);
            }
            let this: Vec<bool> = probe.iter().map(|v| criterion_matches(v, &crit)).collect();
            mask = Some(match mask {
                None => this,
                Some(prev) => prev
                    .iter()
                    .zip(this.iter())
                    .map(|(a, b)| *a && *b)
                    .collect(),
            });
            i += 2;
        }
        Ok(mask.unwrap_or_default())
    }

    /// SUBSTITUTE(text, old, new, [instance]) — replace all occurrences, or
    /// only the `instance`-th (1-based) when given.
    fn substitute(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 3 {
            return CellValue::Error(FormulaError::Error);
        }
        let text = self.eval_expr(&args[0]).to_plain_string();
        let old = self.eval_expr(&args[1]).to_plain_string();
        let new = self.eval_expr(&args[2]).to_plain_string();
        if old.is_empty() {
            return CellValue::Text(text);
        }
        let instance = args
            .get(3)
            .and_then(|a| self.eval_expr(a).as_number())
            .map(|n| n as usize);
        let out = match instance {
            None => text.replace(&old, &new),
            Some(target) if target >= 1 => {
                let mut result = String::new();
                let mut rest = text.as_str();
                let mut count = 0;
                while let Some(pos) = rest.find(&old) {
                    count += 1;
                    if count == target {
                        result.push_str(&rest[..pos]);
                        result.push_str(&new);
                        result.push_str(&rest[pos + old.len()..]);
                        return CellValue::Text(result);
                    }
                    result.push_str(&rest[..pos + old.len()]);
                    rest = &rest[pos + old.len()..];
                }
                result.push_str(rest);
                result
            }
            _ => text,
        };
        CellValue::Text(out)
    }

    /// `REPLACE(old_text`, start, `num_chars`, `new_text`) — positional replacement.
    fn replace_fn(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 4 {
            return CellValue::Error(FormulaError::Error);
        }
        let chars: Vec<char> = self.eval_expr(&args[0]).to_plain_string().chars().collect();
        let start = (self.eval_expr(&args[1]).as_number().unwrap_or(1.0).max(1.0) as usize - 1)
            .min(chars.len());
        let num = self.eval_expr(&args[2]).as_number().unwrap_or(0.0).max(0.0) as usize;
        let new = self.eval_expr(&args[3]).to_plain_string();
        let end = (start + num).min(chars.len());
        let mut out: String = chars[..start].iter().collect();
        out.push_str(&new);
        out.extend(chars[end..].iter());
        CellValue::Text(out)
    }

    /// FIND/SEARCH(needle, haystack, [start]) — 1-based position of `needle`,
    /// case-sensitive for FIND and case-insensitive for SEARCH.
    fn find_fn(&mut self, args: &[Expr], case_sensitive: bool) -> CellValue {
        if args.len() < 2 {
            return CellValue::Error(FormulaError::Error);
        }
        let needle = self.eval_expr(&args[0]).to_plain_string();
        let hay = self.eval_expr(&args[1]).to_plain_string();
        let start = args
            .get(2)
            .and_then(|a| self.eval_expr(a).as_number())
            .map_or(1, |n| n as usize)
            .max(1)
            - 1;
        let (h, n) = if case_sensitive {
            (hay, needle)
        } else {
            (hay.to_lowercase(), needle.to_lowercase())
        };
        let hchars: Vec<char> = h.chars().collect();
        if start > hchars.len() {
            return CellValue::Error(FormulaError::Value);
        }
        let hsub: String = hchars[start..].iter().collect();
        match hsub.find(&n) {
            Some(byte_pos) => {
                let char_off = hsub[..byte_pos].chars().count();
                CellValue::Number((start + char_off + 1) as f64)
            }
            None => CellValue::Error(FormulaError::Value),
        }
    }

    /// TEXTJOIN(delimiter, `ignore_empty`, values…).
    fn textjoin(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 3 {
            return CellValue::Error(FormulaError::Error);
        }
        let delim = self.eval_expr(&args[0]).to_plain_string();
        let ignore_empty = cond_truthy(&self.eval_expr(&args[1]));
        let mut parts = Vec::new();
        for a in &args[2..] {
            for v in self.collect_values(a) {
                if v.is_error() {
                    return v;
                }
                if ignore_empty && v.is_empty() {
                    continue;
                }
                parts.push(v.to_plain_string());
            }
        }
        CellValue::Text(parts.join(&delim))
    }

    /// VLOOKUP(lookup, table, `col_index`, [`range_lookup`]). Exact match when
    /// `range_lookup` is FALSE; otherwise an ascending approximate match.
    fn vlookup(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 3 {
            return CellValue::Error(FormulaError::Error);
        }
        let key = self.eval_expr(&args[0]);
        if key.is_error() {
            return key;
        }
        let Some((tl, br)) = range_bounds(&args[1]) else {
            return CellValue::Error(FormulaError::Ref);
        };
        let Some(col_index) = self.eval_expr(&args[2]).as_number() else {
            return CellValue::Error(FormulaError::Value);
        };
        let col_index = col_index as i64;
        if col_index < 1 {
            return CellValue::Error(FormulaError::Value);
        }
        let target_col = i64::from(tl.0) + col_index - 1;
        if target_col > i64::from(br.0) {
            return CellValue::Error(FormulaError::Ref);
        }
        let approx = args
            .get(3)
            .is_none_or(|a| {
                self.eval_expr(a)
                    .as_number() != Some(0.0)
            });
        let mut best: Option<u32> = None;
        for r in tl.1..=br.1 {
            let probe = self.eval_cell((tl.0, r));
            if approx {
                if lte(&probe, &key) {
                    best = Some(r);
                } else {
                    break;
                }
            } else if values_equal(&probe, &key) {
                return self.eval_cell((target_col as u32, r));
            }
        }
        match best {
            Some(r) => self.eval_cell((target_col as u32, r)),
            None => CellValue::Error(FormulaError::Na),
        }
    }

    /// HLOOKUP(lookup, table, `row_index`, [`range_lookup`]).
    fn hlookup(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 3 {
            return CellValue::Error(FormulaError::Error);
        }
        let key = self.eval_expr(&args[0]);
        if key.is_error() {
            return key;
        }
        let Some((tl, br)) = range_bounds(&args[1]) else {
            return CellValue::Error(FormulaError::Ref);
        };
        let Some(row_index) = self.eval_expr(&args[2]).as_number() else {
            return CellValue::Error(FormulaError::Value);
        };
        let row_index = row_index as i64;
        if row_index < 1 {
            return CellValue::Error(FormulaError::Value);
        }
        let target_row = i64::from(tl.1) + row_index - 1;
        if target_row > i64::from(br.1) {
            return CellValue::Error(FormulaError::Ref);
        }
        let approx = args
            .get(3)
            .is_none_or(|a| {
                self.eval_expr(a)
                    .as_number() != Some(0.0)
            });
        let mut best: Option<u32> = None;
        for c in tl.0..=br.0 {
            let probe = self.eval_cell((c, tl.1));
            if approx {
                if lte(&probe, &key) {
                    best = Some(c);
                } else {
                    break;
                }
            } else if values_equal(&probe, &key) {
                return self.eval_cell((c, target_row as u32));
            }
        }
        match best {
            Some(c) => self.eval_cell((c, target_row as u32)),
            None => CellValue::Error(FormulaError::Na),
        }
    }

    /// INDEX(range, `row_num`, [`col_num`]). A single-row or single-column range
    /// takes one index; a rectangle takes both.
    fn index_fn(&mut self, args: &[Expr]) -> CellValue {
        let Some((tl, br)) = args.first().and_then(range_bounds) else {
            return CellValue::Error(FormulaError::Ref);
        };
        let rows = br.1 - tl.1 + 1;
        let cols = br.0 - tl.0 + 1;
        let a1 = args
            .get(1)
            .and_then(|a| self.eval_expr(a).as_number())
            .unwrap_or(0.0) as i64;
        let a2 = args
            .get(2)
            .and_then(|a| self.eval_expr(a).as_number())
            .map(|n| n as i64);
        let (row_i, col_i) = match a2 {
            Some(c) => (a1, c),
            None if rows == 1 => (1, a1),
            None if cols == 1 => (a1, 1),
            None => (a1, 1),
        };
        if row_i < 1 || col_i < 1 {
            return CellValue::Error(FormulaError::Value);
        }
        let cc = i64::from(tl.0) + col_i - 1;
        let rr = i64::from(tl.1) + row_i - 1;
        if cc > i64::from(br.0) || rr > i64::from(br.1) {
            return CellValue::Error(FormulaError::Ref);
        }
        self.eval_cell((cc as u32, rr as u32))
    }

    /// MATCH(lookup, range, [`match_type`]). `0` exact, `1` largest ≤ (ascending,
    /// default), `-1` smallest ≥ (descending).
    fn match_fn(&mut self, args: &[Expr]) -> CellValue {
        if args.len() < 2 {
            return CellValue::Error(FormulaError::Error);
        }
        let key = self.eval_expr(&args[0]);
        let Some((tl, br)) = range_bounds(&args[1]) else {
            return CellValue::Error(FormulaError::Na);
        };
        let match_type = args
            .get(2)
            .and_then(|a| self.eval_expr(a).as_number())
            .map_or(1, |n| n as i64);
        let cells: Vec<Addr> = if br.1 == tl.1 {
            (tl.0..=br.0).map(|c| (c, tl.1)).collect()
        } else {
            (tl.1..=br.1).map(|r| (tl.0, r)).collect()
        };
        let mut best: Option<usize> = None;
        for (i, addr) in cells.iter().enumerate() {
            let v = self.eval_cell(*addr);
            match match_type {
                0 => {
                    if values_equal(&v, &key) {
                        return CellValue::Number((i + 1) as f64);
                    }
                }
                t if t > 0 => {
                    if lte(&v, &key) {
                        best = Some(i);
                    } else {
                        break;
                    }
                }
                _ => {
                    if lte(&key, &v) {
                        best = Some(i);
                    } else {
                        break;
                    }
                }
            }
        }
        match best {
            Some(i) => CellValue::Number((i + 1) as f64),
            None => CellValue::Error(FormulaError::Na),
        }
    }

    /// Shared body for YEAR/MONTH/DAY — reads the first arg as a serial and
    /// projects out one calendar component.
    fn date_part(&mut self, args: &[Expr], f: impl Fn((i64, u32, u32)) -> f64) -> CellValue {
        let Some(a) = args.first() else {
            return CellValue::Error(FormulaError::Error);
        };
        let v = self.eval_expr(a);
        if v.is_error() {
            return v;
        }
        match v.as_number() {
            Some(n) => CellValue::Number(f(crate::model::serial_to_ymd(n))),
            None => CellValue::Error(FormulaError::Value),
        }
    }

    /// Shared body for HOUR/MINUTE/SECOND (`which` = 0/1/2).
    fn time_part(&mut self, args: &[Expr], which: u8) -> CellValue {
        let Some(a) = args.first() else {
            return CellValue::Error(FormulaError::Error);
        };
        let v = self.eval_expr(a);
        if v.is_error() {
            return v;
        }
        let Some(n) = v.as_number() else {
            return CellValue::Error(FormulaError::Value);
        };
        let (h, mi, s) = time_parts(n);
        CellValue::Number(match which {
            0 => h,
            1 => mi,
            _ => s,
        } as f64)
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
        for (op, len) in [
            ("<=", 2),
            (">=", 2),
            ("<>", 2),
            ("<", 1),
            (">", 1),
            ("=", 1),
        ] {
            if let Some(rest) = s.strip_prefix(op) {
                let _ = len;
                if let Ok(threshold) = rest.trim().parse::<f64>() {
                    let Some(p) = probe.as_number() else {
                        return false;
                    };
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

/// Normalized inclusive rectangle a lookup/index argument refers to. A single
/// `Ref` is a 1×1 rectangle; anything that isn't a reference yields `None`.
fn range_bounds(arg: &Expr) -> Option<(Addr, Addr)> {
    match arg {
        Expr::Range(a, b) => Some(((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)))),
        Expr::Ref(a) => Some((*a, *a)),
        _ => None,
    }
}

/// Equality used by lookups and SWITCH: numeric when both coerce, else a
/// case-insensitive text compare (matching spreadsheet lookup semantics).
fn values_equal(a: &CellValue, b: &CellValue) -> bool {
    match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) => x == y,
        _ => a
            .to_plain_string()
            .eq_ignore_ascii_case(&b.to_plain_string()),
    }
}

/// `a <= b` for approximate lookup/match: numeric when both coerce, else a
/// case-insensitive lexical compare.
fn lte(a: &CellValue, b: &CellValue) -> bool {
    match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) => x <= y,
        _ => a
            .to_plain_string()
            .to_lowercase()
            .le(&b.to_plain_string().to_lowercase()),
    }
}

/// Whether a value counts as "true" for IF/IFS/TEXTJOIN's ignore-empty flag.
fn cond_truthy(v: &CellValue) -> bool {
    match v {
        CellValue::Bool(b) => *b,
        CellValue::Number(n) => *n != 0.0,
        CellValue::Empty => false,
        CellValue::Text(t) => match t.to_ascii_uppercase().as_str() {
            "TRUE" => true,
            "FALSE" => false,
            _ => !t.is_empty(),
        },
        CellValue::Error(_) => false,
    }
}

/// The `(hours, minutes, seconds)` of a serial's fractional day.
fn time_parts(serial: f64) -> (i64, i64, i64) {
    let frac = serial.fract().abs();
    let total = (frac * 86400.0).round() as i64;
    ((total / 3600) % 24, (total % 3600) / 60, total % 60)
}

/// Title-case a string: capitalize the first letter of each run of letters.
fn proper_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_alpha = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if prev_alpha {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_alpha = true;
        } else {
            out.push(c);
            prev_alpha = false;
        }
    }
    out
}

/// The full English month name for `TEXT`'s `mmmm` token.
fn full_month(m: u32) -> &'static str {
    const M: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    M.get((m.max(1).min(12) - 1) as usize)
        .copied()
        .unwrap_or("January")
}

/// Render a number through a `TEXT` format code — a date/time picture when the
/// code carries date letters, otherwise a numeric picture.
fn apply_text_code(n: f64, code: &str) -> String {
    let lc = code.to_lowercase();
    let is_date = lc.contains('y') || lc.contains('d') || lc.contains('h') || lc.contains("mmm");
    if is_date {
        format_date_code(n, code)
    } else {
        format_number_code(n, code)
    }
}

/// A number through a numeric `TEXT` picture: decimals come from digits after
/// the `.`, `,` turns on thousands grouping, `%` scales by 100, a leading `$`
/// prefixes the currency sign.
fn format_number_code(n: f64, code: &str) -> String {
    let percent = code.contains('%');
    let value = if percent { n * 100.0 } else { n };
    let grouping = code.contains(',');
    let decimals = code
        .split('.')
        .nth(1)
        .map_or(0, |frac| frac.chars().filter(|c| *c == '0' || *c == '#').count());
    let mut out = if grouping {
        crate::model::format_thousands(value, decimals)
    } else {
        format!("{value:.decimals$}")
    };
    if percent {
        out.push('%');
    }
    if code.trim_start().starts_with('$') {
        out.insert(0, '$');
    }
    out
}

/// A serial through a date/time `TEXT` picture. Supports `yyyy`/`yy`,
/// `mmmm`/`mmm`/`mm`/`m` (month, or minute in a time context), `dd`/`d`,
/// `hh`/`h`, `ss`, and `am/pm`; anything else is copied through literally.
fn format_date_code(serial: f64, code: &str) -> String {
    let (y, m, d) = crate::model::serial_to_ymd(serial);
    let (hh, mi, ss) = time_parts(serial);
    let lc: Vec<char> = code.to_lowercase().chars().collect();
    let has_ampm = code.to_lowercase().contains("am/pm");
    let disp_h = if has_ampm {
        match hh % 12 {
            0 => 12,
            h => h,
        }
    } else {
        hh
    };
    let mut out = String::new();
    let mut i = 0;
    // "mm"/"m" mean minutes only when we've just passed an hour token / colon.
    let mut time_ctx = false;
    let starts = |from: usize, p: &str| lc[from..].iter().collect::<String>().starts_with(p);
    while i < lc.len() {
        if starts(i, "am/pm") {
            out.push_str(if hh < 12 { "AM" } else { "PM" });
            i += 5;
        } else if starts(i, "yyyy") {
            out.push_str(&format!("{y:04}"));
            i += 4;
            time_ctx = false;
        } else if starts(i, "yy") {
            out.push_str(&format!("{:02}", (y % 100 + 100) % 100));
            i += 2;
            time_ctx = false;
        } else if starts(i, "mmmm") {
            out.push_str(full_month(m));
            i += 4;
        } else if starts(i, "mmm") {
            out.push_str(month_abbrev_local(m));
            i += 3;
        } else if starts(i, "hh") {
            out.push_str(&format!("{disp_h:02}"));
            i += 2;
            time_ctx = true;
        } else if starts(i, "h") {
            out.push_str(&format!("{disp_h}"));
            i += 1;
            time_ctx = true;
        } else if starts(i, "ss") {
            out.push_str(&format!("{ss:02}"));
            i += 2;
        } else if starts(i, "dd") {
            out.push_str(&format!("{d:02}"));
            i += 2;
            time_ctx = false;
        } else if starts(i, "d") {
            out.push_str(&format!("{d}"));
            i += 1;
            time_ctx = false;
        } else if starts(i, "mm") {
            if time_ctx {
                out.push_str(&format!("{mi:02}"));
                time_ctx = false;
            } else {
                out.push_str(&format!("{m:02}"));
            }
            i += 2;
        } else if starts(i, "m") {
            if time_ctx {
                out.push_str(&format!("{mi}"));
                time_ctx = false;
            } else {
                out.push_str(&format!("{m}"));
            }
            i += 1;
        } else {
            let ch = lc[i];
            if ch == ':' {
                time_ctx = true;
            }
            out.push(ch);
            i += 1;
        }
    }
    out
}

/// Month abbreviation for the `mmm` date token (kept local to the engine so
/// the model's private helper needn't be exported).
fn month_abbrev_local(m: u32) -> &'static str {
    const M: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    M.get((m.max(1).min(12) - 1) as usize)
        .copied()
        .unwrap_or("Jan")
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
    if let Some(p) = trimmed.strip_suffix('%')
        && let Ok(n) = p.trim().parse::<f64>() {
            return CellValue::Number(n / 100.0);
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
pub fn infer_result_format(body: &str, formats: &HashMap<Addr, CellFormat>) -> Option<CellFormat> {
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
        Expr::Ref(addr) => formats
            .get(addr)
            .map_or(Unit::Scalar, |f| unit_of_format(*f)),
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
    use BinOp::{Eq, Ne, Lt, Gt, Le, Ge, Concat, Div, Mul, Pow, Add, Sub};
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
        // Date/time builders render as dates/times so a bare `=TODAY()` shows
        // a date rather than its underlying serial number.
        "DATE" | "TODAY" | "NOW" => Unit::Date(crate::model::DateStyle::Iso),
        "TIME" => Unit::Time(crate::model::TimeStyle::H24),
        // Counting / text / logical / lookup functions are unitless.
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
                    out.push(
                        formats
                            .get(&(c, r))
                            .map_or(Unit::Scalar, |f| unit_of_format(*f)),
                    );
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
        map.get(&a1_to_addr(a).unwrap())
            .cloned()
            .unwrap_or(CellValue::Empty)
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
        assert_eq!(val(&m, "A1"), CellValue::Error(FormulaError::Div0));
        assert!(val(&m, "B1").is_error());
    }

    #[test]
    fn specific_excel_error_codes() {
        use crate::model::{CellFormat, format_value};
        // Each formula triggers one specific cause. Assert both the typed
        // error kind *and* the exact canonical string the grid renders
        // (through the real `recalculate` + `format_value` path).
        let m = run(&[
            ("A1", "=1/0"),                // division by zero        → #DIV/0!
            ("A2", "=FOOBAR()"),           // unknown function        → #NAME?
            ("A3", "=NOTACELL"),           // unrecognized bare name  → #NAME?
            ("A4", "=VLOOKUP(1,B1:B3,5)"), // column outside table    → #REF!
            ("A5", "=\"a\"+1"),            // text where number wanted → #VALUE!
            ("A6", "=MATCH(999,B1:B3,0)"), // lookup with no match    → #N/A
            ("B1", "10"),
            ("B2", "20"),
            ("B3", "30"),
        ]);
        let cases = [
            ("A1", FormulaError::Div0, "#DIV/0!"),
            ("A2", FormulaError::Name, "#NAME?"),
            ("A3", FormulaError::Name, "#NAME?"),
            ("A4", FormulaError::Ref, "#REF!"),
            ("A5", FormulaError::Value, "#VALUE!"),
            ("A6", FormulaError::Na, "#N/A"),
        ];
        for (cell, kind, text) in cases {
            let v = val(&m, cell);
            assert_eq!(v, CellValue::Error(kind), "{cell} error kind");
            assert_eq!(
                format_value(&v, CellFormat::General),
                text,
                "{cell} rendered as canonical Excel code",
            );
        }
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

    #[test]
    fn math_functions() {
        let m = run(&[
            ("A1", "=POWER(2,10)"),
            ("A2", "=MOD(17,5)"),
            ("A3", "=SQRT(144)"),
            ("A4", "=ROUND(3.14159,2)"),
            ("A5", "=ABS(-8)"),
            ("A6", "=INT(7.9)"),
            ("A7", "=PRODUCT(2,3,4)"),
            ("A8", "=TRUNC(9.99)"),
            ("A9", "=LOG(1000)"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Number(1024.0));
        assert_eq!(val(&m, "A2"), CellValue::Number(2.0));
        assert_eq!(val(&m, "A3"), CellValue::Number(12.0));
        assert_eq!(val(&m, "A4"), CellValue::Number(3.14));
        assert_eq!(val(&m, "A5"), CellValue::Number(8.0));
        assert_eq!(val(&m, "A6"), CellValue::Number(7.0));
        assert_eq!(val(&m, "A7"), CellValue::Number(24.0));
        assert_eq!(val(&m, "A8"), CellValue::Number(9.0));
        // LOG(1000) is 3 up to floating-point rounding.
        match val(&m, "A9") {
            CellValue::Number(n) => assert!((n - 3.0).abs() < 1e-9, "LOG(1000)={n}"),
            other => panic!("expected a number, got {other:?}"),
        }
    }

    #[test]
    fn stats_conditional_functions() {
        let m = run(&[
            ("A1", "10"),
            ("A2", "20"),
            ("A3", "30"),
            ("A4", "40"),
            ("B1", "x"),
            ("B2", "y"),
            ("B3", "x"),
            ("B4", "y"),
            ("C1", "=AVERAGEIF(A1:A4,\">15\")"),
            ("C2", "=SUMIFS(A1:A4,B1:B4,\"x\")"),
            ("C3", "=COUNTIFS(A1:A4,\">=20\",B1:B4,\"y\")"),
            ("C4", "=AVERAGEIFS(A1:A4,B1:B4,\"y\")"),
        ]);
        // Average of 20,30,40 = 30.
        assert_eq!(val(&m, "C1"), CellValue::Number(30.0));
        // Sum of A where B="x": 10 + 30 = 40.
        assert_eq!(val(&m, "C2"), CellValue::Number(40.0));
        // A>=20 AND B="y": A2(20,y) and A4(40,y) → 2.
        assert_eq!(val(&m, "C3"), CellValue::Number(2.0));
        // Average of A where B="y": (20+40)/2 = 30.
        assert_eq!(val(&m, "C4"), CellValue::Number(30.0));
    }

    #[test]
    fn logical_functions() {
        let m = run(&[
            ("A1", "=IFERROR(1/0,\"safe\")"),
            ("A2", "=IFS(FALSE,1,TRUE,2)"),
            ("A3", "=XOR(TRUE,TRUE,TRUE)"),
            ("A4", "=SWITCH(3,1,\"a\",3,\"c\",\"z\")"),
            ("A5", "=IF(AND(1>0,2>1),\"yes\",\"no\")"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Text("safe".into()));
        assert_eq!(val(&m, "A2"), CellValue::Number(2.0));
        assert_eq!(val(&m, "A3"), CellValue::Bool(true));
        assert_eq!(val(&m, "A4"), CellValue::Text("c".into()));
        assert_eq!(val(&m, "A5"), CellValue::Text("yes".into()));
    }

    #[test]
    fn extended_text_functions() {
        let m = run(&[
            ("A1", "=SUBSTITUTE(\"a-b-c\",\"-\",\"+\")"),
            ("A2", "=PROPER(\"hello world\")"),
            ("A3", "=FIND(\"b\",\"abc\")"),
            ("A4", "=SEARCH(\"B\",\"abc\")"),
            ("A5", "=REPT(\"ab\",3)"),
            ("A6", "=TEXTJOIN(\",\",TRUE,\"x\",\"y\",\"z\")"),
            ("A7", "=REPLACE(\"abcdef\",2,3,\"XY\")"),
            ("A8", "=VALUE(\"1,234\")"),
            ("A9", "=EXACT(\"Ab\",\"ab\")"),
        ]);
        assert_eq!(val(&m, "A1"), CellValue::Text("a+b+c".into()));
        assert_eq!(val(&m, "A2"), CellValue::Text("Hello World".into()));
        assert_eq!(val(&m, "A3"), CellValue::Number(2.0));
        assert_eq!(val(&m, "A4"), CellValue::Number(2.0));
        assert_eq!(val(&m, "A5"), CellValue::Text("ababab".into()));
        assert_eq!(val(&m, "A6"), CellValue::Text("x,y,z".into()));
        assert_eq!(val(&m, "A7"), CellValue::Text("aXYef".into()));
        assert_eq!(val(&m, "A8"), CellValue::Number(1234.0));
        assert_eq!(val(&m, "A9"), CellValue::Bool(false));
    }

    #[test]
    fn lookup_functions() {
        // A small table: A=key, B=payload.
        let m = run(&[
            ("A1", "1"),
            ("B1", "one"),
            ("A2", "2"),
            ("B2", "two"),
            ("A3", "3"),
            ("B3", "three"),
            ("D1", "=VLOOKUP(2,A1:B3,2,FALSE)"),
            ("D2", "=INDEX(B1:B3,3)"),
            ("D3", "=MATCH(3,A1:A3,0)"),
            ("D4", "=HLOOKUP(2,A1:C1,1,FALSE)"),
            ("C1", "hit"),
            ("D5", "=CHOOSE(2,\"a\",\"b\",\"c\")"),
        ]);
        assert_eq!(val(&m, "D1"), CellValue::Text("two".into()));
        assert_eq!(val(&m, "D2"), CellValue::Text("three".into()));
        assert_eq!(val(&m, "D3"), CellValue::Number(3.0));
        assert_eq!(val(&m, "D5"), CellValue::Text("b".into()));
    }

    #[test]
    fn date_time_functions() {
        let m = run(&[
            ("A1", "=DATE(2023,3,15)"),
            ("A2", "=YEAR(A1)"),
            ("A3", "=MONTH(A1)"),
            ("A4", "=DAY(A1)"),
            ("A5", "=WEEKDAY(A1)"),
            ("A6", "=HOUR(TIME(14,5,0))"),
            ("A7", "=MINUTE(TIME(14,5,0))"),
        ]);
        // The serial must round-trip through the model helpers.
        let serial = crate::model::date_to_serial(2023, 3, 15);
        assert_eq!(val(&m, "A1"), CellValue::Number(serial));
        assert_eq!(val(&m, "A2"), CellValue::Number(2023.0));
        assert_eq!(val(&m, "A3"), CellValue::Number(3.0));
        assert_eq!(val(&m, "A4"), CellValue::Number(15.0));
        // 2023-03-15 is a Wednesday → 4 in Excel's Sunday-first numbering.
        assert_eq!(val(&m, "A5"), CellValue::Number(4.0));
        assert_eq!(val(&m, "A6"), CellValue::Number(14.0));
        assert_eq!(val(&m, "A7"), CellValue::Number(5.0));
    }

    #[test]
    fn today_returns_a_plausible_recent_serial() {
        let m = run(&[("A1", "=TODAY()"), ("A2", "=NOW()")]);
        // A serial well past 2020-01-01 (43831) and before 2100.
        match val(&m, "A1") {
            CellValue::Number(n) => assert!(n > 43831.0 && n < 73415.0, "today serial {n}"),
            other => panic!("TODAY must be a number, got {other:?}"),
        }
        match val(&m, "A2") {
            CellValue::Number(n) => assert!(n > 43831.0, "now serial {n}"),
            other => panic!("NOW must be a number, got {other:?}"),
        }
    }

    #[test]
    fn text_format_codes() {
        assert_eq!(apply_text_code(1234.5, "#,##0.00"), "1,234.50");
        assert_eq!(apply_text_code(0.1234, "0.0%"), "12.3%");
        let serial = crate::model::date_to_serial(2023, 3, 5);
        assert_eq!(apply_text_code(serial, "yyyy-mm-dd"), "2023-03-05");
        assert_eq!(apply_text_code(serial, "d mmm yyyy"), "5 Mar 2023");
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

    // --- reference extraction -----------------------------------------------

    #[test]
    fn references_single_cells_get_spans_and_targets() {
        // Body of `=B9-B7`: two single-cell refs, distinct colour slots.
        let spans = references("B9-B7");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].tl, a1_to_addr("B9").unwrap());
        assert_eq!(spans[0].br, a1_to_addr("B9").unwrap());
        assert_eq!(spans[1].tl, a1_to_addr("B7").unwrap());
        // The spans point at the actual characters `B9` and `B7`.
        assert_eq!(&"B9-B7"[spans[0].start..spans[0].end], "B9");
        assert_eq!(&"B9-B7"[spans[1].start..spans[1].end], "B7");
        let slots = ref_color_slots(&spans);
        assert_eq!(slots, vec![0, 1], "two different refs → two colours");
    }

    #[test]
    fn references_expand_ranges_and_share_colour_for_duplicates() {
        // `SUM(B2:D2)+B2`: the range B2:D2 and the single B2 are different
        // keys; a repeated B2 would reuse a slot.
        let spans = references("SUM(B2:D2)+B2");
        assert_eq!(spans.len(), 2, "SUM is a call, not a ref");
        assert_eq!(spans[0].key, "B2:D2");
        assert_eq!(spans[0].tl, a1_to_addr("B2").unwrap());
        assert_eq!(spans[0].br, a1_to_addr("D2").unwrap());
        assert_eq!(spans[1].key, "B2");

        // A ref repeated twice keeps one colour slot.
        let dup = references("A1+A1");
        let slots = ref_color_slots(&dup);
        assert_eq!(slots, vec![0, 0]);
    }

    #[test]
    fn references_skip_strings_and_functions() {
        // `IF(B7>B9,"A1","B2")`: refs are B7 and B9 only; the quoted A1/B2 are
        // string literals and IF is a function name.
        let spans = references("IF(B7>B9,\"A1\",\"B2\")");
        let keys: Vec<&str> = spans.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["B7", "B9"]);
    }

    // --- reference adjustment (copy/paste + fill) ---------------------------

    #[test]
    fn offset_refs_shifts_relative_preserves_absolute() {
        // A bare reference shifts on the pasted-to axis. Copying `=A1` from B2
        // and pasting into B3 (Δrow +1) yields `=A2`; into C2 (Δcol +1) → `=B1`.
        assert_eq!(offset_refs("=A1", 0, 1), "=A2");
        assert_eq!(offset_refs("=A1", 1, 0), "=B1");
        assert_eq!(offset_refs("=A1", 1, 1), "=B2");

        // `$A$1` is fully anchored and never moves anywhere.
        assert_eq!(offset_refs("=$A$1", 3, 4), "=$A$1");
        // `$A1` locks the column: only the row shifts.
        assert_eq!(offset_refs("=$A1", 5, 2), "=$A3");
        // `A$1` locks the row: only the column shifts.
        assert_eq!(offset_refs("=A$1", 2, 5), "=C$1");

        // Ranges shift each endpoint independently.
        assert_eq!(offset_refs("=SUM(A1:B3)", 1, 1), "=SUM(B2:C4)");
        // A range mixing anchored and relative endpoints shifts only the free one.
        assert_eq!(offset_refs("=$A$1:B2", 1, 1), "=$A$1:C3");
    }

    #[test]
    fn offset_refs_leaves_functions_strings_and_numbers_alone() {
        // Function names and quoted string literals are never references.
        assert_eq!(
            offset_refs("=IF(A1>0,\"A1\",B2)", 0, 1),
            "=IF(A2>0,\"A1\",B3)"
        );
        // Scientific-notation literals (`1e3`) are numbers, not references.
        assert_eq!(offset_refs("=1e3+A1", 0, 1), "=1e3+A2");
        // A plain literal (not a formula) is returned unchanged.
        assert_eq!(offset_refs("1200", 3, 3), "1200");
        // A zero offset is an identity, formula or not.
        assert_eq!(offset_refs("=A1+B2", 0, 0), "=A1+B2");
        // A relative component never underflows past the grid origin.
        assert_eq!(offset_refs("=A1", -5, -5), "=A1");
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
