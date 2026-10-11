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
use std::fmt::Write as _;
use std::hash::BuildHasher;

use crate::model::{Addr, CellFormat, CellValue, FormulaError, GridEdit, a1_to_addr, addr_to_a1};

// ---------------------------------------------------------------------------
// Character cursor
// ---------------------------------------------------------------------------

/// A bounds-checked cursor over a formula's characters.
///
/// The reference scanner, the tokenizer and the `TEXT` date picture all walk a
/// formula by index. Routing every read through one cursor keeps the index
/// inside the buffer, so a truncated or malformed formula ends the scan
/// instead of reading past the end. `Copy`, so a caller can fork a look-ahead
/// and commit it only once the look-ahead has succeeded.
#[derive(Clone, Copy)]
struct Scan<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> Scan<'a> {
    fn new(chars: &'a [char]) -> Self {
        Self { chars, pos: 0 }
    }

    /// Index of the next character to read.
    fn pos(self) -> usize {
        self.pos
    }

    /// The next character, without consuming it.
    fn peek(self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// The character immediately before `index`, if there is one.
    fn before(self, index: usize) -> Option<char> {
        index
            .checked_sub(1)
            .and_then(|prev| self.chars.get(prev))
            .copied()
    }

    /// Consume and return the next character.
    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos = self.pos.saturating_add(1);
        Some(c)
    }

    /// Skip `n` characters, stopping at the end of the buffer.
    fn advance(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n).min(self.chars.len());
    }

    /// Consume characters for as long as `pred` holds.
    fn skip_while(&mut self, pred: impl Fn(char) -> bool) {
        while self.peek().is_some_and(&pred) {
            self.pos = self.pos.saturating_add(1);
        }
    }

    /// The text between `start` and the cursor.
    fn text_from(self, start: usize) -> String {
        self.chars
            .get(start..self.pos)
            .unwrap_or_default()
            .iter()
            .collect()
    }

    /// Whether the text at the cursor begins with `needle`.
    fn starts_with(self, needle: &str) -> bool {
        let mut rest = self.chars.get(self.pos..).unwrap_or_default().iter();
        needle.chars().all(|want| rest.next() == Some(&want))
    }
}

// ---------------------------------------------------------------------------
// Numeric conversions
// ---------------------------------------------------------------------------

/// A spreadsheet number as an integer, truncating toward zero the way Excel
/// does. This is the file's single float-to-integer conversion: every index,
/// count and component derived from a formula value goes through it, so an
/// absurd argument clamps instead of wrapping.
#[expect(
    clippy::cast_possible_truncation,
    reason = "float-to-int `as` saturates at i64's bounds and maps NaN to 0"
)]
fn number_to_int(n: f64) -> i64 {
    n.trunc() as i64
}

/// A spreadsheet number as a 0-based index, clamped into `0..=len`. Negative,
/// NaN and oversized arguments collapse to the nearest valid position.
fn number_to_index(n: f64, len: usize) -> usize {
    usize::try_from(number_to_int(n)).unwrap_or(0).min(len)
}

/// A count or length as a spreadsheet number. Counts come from materialised
/// ranges, so `u32` is far above any reachable value and the conversion stays
/// exact.
fn count_number(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// An integer as a spreadsheet number. Values outside `i32` clamp, which keeps
/// the conversion exact for every calendar and index quantity that reaches it.
fn int_number(n: i64) -> f64 {
    f64::from(i32::try_from(n.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0))
}

/// The median of a sample, or `None` when the sample is empty. Sorts in place
/// with `total_cmp`, so a NaN orders instead of aborting the comparison.
fn median(sample: &mut [f64]) -> Option<f64> {
    sample.sort_by(f64::total_cmp);
    let mid = sample.len() / 2;
    let hi = *sample.get(mid)?;
    if !sample.len().is_multiple_of(2) {
        return Some(hi);
    }
    let lo = *mid.checked_sub(1).and_then(|i| sample.get(i))?;
    Some(f64::midpoint(lo, hi))
}

/// Which component `HOUR`/`MINUTE`/`SECOND` projects out of a serial.
#[derive(Clone, Copy)]
enum TimeComponent {
    Hour,
    Minute,
    Second,
}

/// Exact equality between two spreadsheet numbers.
#[expect(
    clippy::float_cmp,
    reason = "spreadsheet `=` compares values exactly, as Excel does; an epsilon would make distinct cells compare equal"
)]
fn numbers_equal(a: f64, b: f64) -> bool {
    a == b
}

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
    let mut scan = Scan::new(&chars);
    let mut out = Vec::new();
    while let Some(ch) = scan.peek() {
        if ch == '"' {
            scan.bump();
            scan.skip_while(|c| c != '"');
            scan.bump(); // closing quote, when the literal is terminated
            continue;
        }
        if ch != '$' && !ch.is_ascii_alphabetic() {
            scan.bump();
            continue;
        }
        let start = scan.pos();
        scan.skip_while(|c| c == '$' || c.is_ascii_alphanumeric());
        // A run right after a digit/dot is an exponent (`1e3`), not a ref.
        let prev_numish = scan
            .before(start)
            .is_some_and(|prev| prev.is_ascii_digit() || prev == '.');
        // An identifier followed by `(` is a function name, not a ref.
        let mut ahead = scan;
        ahead.skip_while(char::is_whitespace);
        if prev_numish || ahead.peek() == Some('(') {
            continue;
        }
        let Some(first) = a1_to_addr(&scan.text_from(start)) else {
            continue; // TRUE / FALSE / function names / bare words
        };
        // A `:` (optionally spaced) followed by another reference makes a
        // range that spans from the first reference to the second.
        if ahead.peek() == Some(':') {
            ahead.bump();
            ahead.skip_while(char::is_whitespace);
            let tail_start = ahead.pos();
            ahead.skip_while(|c| c == '$' || c.is_ascii_alphanumeric());
            if let Some(second) = a1_to_addr(&ahead.text_from(tail_start)) {
                let tl = (first.0.min(second.0), first.1.min(second.1));
                let br = (first.0.max(second.0), first.1.max(second.1));
                let key = format!("{}:{}", addr_to_a1(tl.0, tl.1), addr_to_a1(br.0, br.1));
                out.push(RefSpan {
                    start,
                    end: ahead.pos(),
                    tl,
                    br,
                    key,
                });
                scan = ahead;
                continue;
            }
        }
        let key = addr_to_a1(first.0, first.1);
        out.push(RefSpan {
            start,
            end: scan.pos(),
            tl: first,
            br: first,
            key,
        });
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
    if dcol == 0 && drow == 0 {
        return raw.to_string();
    }
    let shift = |token: RefToken| token.render(token.offset(dcol, drow));
    rewrite_refs(raw, |first, second| match second {
        Some((sep, second)) => format!("{}{sep}{}", shift(first), shift(second)),
        None => shift(first),
    })
}

/// Rewrite every A1 reference in a formula through a structural grid change.
///
/// Unlike [`offset_refs`], this is not a copy/fill operation: `$` anchors
/// affect copying, but inserting or deleting a row/column moves *all*
/// references, including absolute ones. A target that was deleted becomes the
/// canonical `#REF!` token. A range `A1:B3` moves as a unit through
/// [`GridEdit::map_range`], so deleting its first or last line shrinks it and
/// only deleting all of it yields `#REF!`. String literals, function names,
/// and numeric exponents use the same scanner as copy/fill and are left
/// verbatim.
///
/// Keeping this beside [`offset_refs`] is important: row/column actions must
/// preserve formula relationships in the same replayable action that moves the
/// cells, rather than leaving stale coordinates for a later UI-only repair.
pub fn remap_refs(raw: &str, edit: GridEdit) -> String {
    rewrite_refs(raw, |first, second| {
        let moved = match second {
            Some((sep, second)) => edit
                .map_range(first.addr, second.addr)
                .map(|(a, b)| format!("{}{sep}{}", first.render(a), second.render(b))),
            None => edit.map_addr(first.addr).map(|addr| first.render(addr)),
        };
        moved.unwrap_or_else(|| "#REF!".into())
    })
}

/// Rewrite every reference in a formula through `rewrite`, which receives a
/// lone reference or both ends of a range with the separator between them.
/// String literals, function names, and numeric exponents (`1e3`) are copied
/// verbatim; non-formula input (no leading `=`) is returned unchanged.
fn rewrite_refs(
    raw: &str,
    mut rewrite: impl FnMut(RefToken, Option<(&str, RefToken)>) -> String,
) -> String {
    let Some(body) = raw.strip_prefix('=') else {
        return raw.to_string();
    };
    let chars: Vec<char> = body.chars().collect();
    let mut scan = Scan::new(&chars);
    let mut out = String::with_capacity(raw.len().saturating_add(8));
    out.push('=');
    while let Some(ch) = scan.peek() {
        if ch == '"' {
            // Copy a string literal verbatim, including the closing quote.
            let start = scan.pos();
            scan.bump();
            scan.skip_while(|c| c != '"');
            scan.bump();
            out.push_str(&scan.text_from(start));
            continue;
        }
        if ch != '$' && !ch.is_ascii_alphabetic() {
            out.push(ch);
            scan.bump();
            continue;
        }
        let start = scan.pos();
        scan.skip_while(|c| c == '$' || c.is_ascii_alphanumeric());
        let token = scan.text_from(start);
        // A run right after a digit/dot is an exponent (`1e3`), not a ref.
        let prev_numish = scan
            .before(start)
            .is_some_and(|prev| prev.is_ascii_digit() || prev == '.');
        // An identifier immediately followed by `(` is a function name.
        let mut ahead = scan;
        ahead.skip_while(char::is_whitespace);
        let is_call = ahead.peek() == Some('(');
        let Some(first) = RefToken::parse(&token).filter(|_| !prev_numish && !is_call) else {
            out.push_str(&token);
            continue;
        };
        if ahead.peek() == Some(':') {
            ahead.bump();
            ahead.skip_while(char::is_whitespace);
            let sep = ahead.text_from(scan.pos());
            let end_start = ahead.pos();
            ahead.skip_while(|c| c == '$' || c.is_ascii_alphanumeric());
            if let Some(second) = RefToken::parse(&ahead.text_from(end_start)) {
                scan = ahead;
                out.push_str(&rewrite(first, Some((&sep, second))));
                continue;
            }
        }
        out.push_str(&rewrite(first, None));
    }
    out
}

/// One `A1`/`$A$1`-shaped token, remembering its `$` spelling so a
/// structural remap can write the moved address back the same way.
#[derive(Clone, Copy)]
struct RefToken {
    addr: Addr,
    col_abs: bool,
    row_abs: bool,
}

impl RefToken {
    /// `None` when the token is not a well-formed reference.
    fn parse(tok: &str) -> Option<Self> {
        let bytes = tok.as_bytes();
        let mut at = 0usize;
        let col_abs = bytes.first() == Some(&b'$');
        if col_abs {
            at = 1;
        }
        let letters_start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_alphabetic) {
            at = at.saturating_add(1);
        }
        let letters = tok.get(letters_start..at).filter(|s| !s.is_empty())?;
        let row_abs = bytes.get(at) == Some(&b'$');
        if row_abs {
            at = at.saturating_add(1);
        }
        let digits_start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at = at.saturating_add(1);
        }
        if at != bytes.len() {
            return None;
        }
        let addr = (
            crate::model::letters_to_col(letters)?,
            tok.get(digits_start..at)?
                .parse::<u32>()
                .ok()?
                .checked_sub(1)?,
        );
        Some(Self {
            addr,
            col_abs,
            row_abs,
        })
    }

    /// The address moved by `(dcol, drow)` for a copy or fill: `$`-anchored
    /// components stay, the others clamp to the grid's addressable range
    /// so a wild offset lands on the edge rather than wrapping around it.
    fn offset(self, dcol: i64, drow: i64) -> Addr {
        let shift = |at: u32, by: i64, anchored: bool| {
            if anchored {
                return at;
            }
            let moved = i64::from(at)
                .saturating_add(by)
                .clamp(0, i64::from(u32::MAX));
            u32::try_from(moved).unwrap_or(at)
        };
        (
            shift(self.addr.0, dcol, self.col_abs),
            shift(self.addr.1, drow, self.row_abs),
        )
    }

    /// `addr` spelled with this token's `$` anchors.
    fn render(self, (col, row): Addr) -> String {
        let mut result = String::new();
        if self.col_abs {
            result.push('$');
        }
        result.push_str(&crate::model::col_to_letters(col));
        if self.row_abs {
            result.push('$');
        }
        result.push_str(&row.saturating_add(1).to_string());
        result
    }
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
    let mut scan = Scan::new(&chars);
    let mut out = Vec::new();
    while let Some(ch) = scan.peek() {
        if ch.is_whitespace() {
            scan.bump();
            continue;
        }
        match ch {
            '0'..='9' | '.' => {
                let start = scan.pos();
                scan.skip_while(|c| c.is_ascii_digit() || c == '.');
                // optional scientific exponent: 1e3, 2.5E-4
                if matches!(scan.peek(), Some('e' | 'E')) {
                    let before_exponent = scan;
                    scan.bump();
                    if matches!(scan.peek(), Some('+' | '-')) {
                        scan.bump();
                    }
                    if scan.peek().is_some_and(|c| c.is_ascii_digit()) {
                        scan.skip_while(|c| c.is_ascii_digit());
                    } else {
                        scan = before_exponent; // not actually an exponent
                    }
                }
                let n = scan
                    .text_from(start)
                    .parse::<f64>()
                    .map_err(|_| FormulaError::Error)?;
                out.push(Token::Num(n));
            }
            '"' => {
                scan.bump();
                let start = scan.pos();
                scan.skip_while(|c| c != '"');
                let literal = scan.text_from(start);
                if scan.bump() != Some('"') {
                    return Err(FormulaError::Error); // unterminated string
                }
                out.push(Token::Str(literal));
            }
            '$' | 'A'..='Z' | 'a'..='z' | '_' => {
                // Identifier: function name or cell reference (may contain $).
                let start = scan.pos();
                scan.skip_while(|c| c.is_ascii_alphanumeric() || c == '$' || c == '_' || c == '.');
                out.push(Token::Ident(scan.text_from(start)));
            }
            '<' => {
                scan.bump();
                match scan.peek() {
                    Some('=') => {
                        scan.bump();
                        out.push(Token::Le);
                    }
                    Some('>') => {
                        scan.bump();
                        out.push(Token::Ne);
                    }
                    _ => out.push(Token::Lt),
                }
            }
            '>' => {
                scan.bump();
                if scan.peek() == Some('=') {
                    scan.bump();
                    out.push(Token::Ge);
                } else {
                    out.push(Token::Gt);
                }
            }
            '=' => {
                scan.bump();
                out.push(Token::Eq);
            }
            '+' | '-' | '*' | '/' | '^' | '&' | '%' | '(' | ')' | ',' | ':' => {
                scan.bump();
                out.push(Token::Op(ch));
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
            self.pos = self.pos.saturating_add(1);
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
                    let Some(Token::Ident(end)) = self.next() else {
                        return Err(FormulaError::Ref);
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
pub struct Engine<'a, S = std::collections::hash_map::RandomState> {
    raw: &'a HashMap<Addr, String, S>,
    cache: HashMap<Addr, CellValue>,
    visiting: HashSet<Addr>,
}

impl<'a, S: BuildHasher> Engine<'a, S> {
    pub fn new(raw: &'a HashMap<Addr, String, S>) -> Self {
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
            return eval_compare(op, &lv, &rv);
        }
        let (Some(a), Some(b)) = (lv.as_number(), rv.as_number()) else {
            return CellValue::Error(FormulaError::Value);
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
            // Concat and the comparisons are handled above.
            BinOp::Concat
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Gt
            | BinOp::Le
            | BinOp::Ge => return CellValue::Error(FormulaError::Value),
        };
        CellValue::Number(result)
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

    /// Dispatch a function call to the family that implements it. Each stage
    /// returns `None` for a name it does not own, so an unknown function falls
    /// through to `#NAME?`.
    fn eval_call(&mut self, name: &str, args: &[Expr]) -> CellValue {
        if let Some(v) = self.eval_aggregate(name, args) {
            return v;
        }
        if let Some(v) = self.eval_logical(name, args) {
            return v;
        }
        if let Some(v) = self.eval_math(name, args) {
            return v;
        }
        if let Some(v) = self.eval_text(name, args) {
            return v;
        }
        if let Some(v) = self.eval_lookup(name, args) {
            return v;
        }
        if let Some(v) = self.eval_datetime(name, args) {
            return v;
        }
        CellValue::Error(FormulaError::Name)
    }

    /// How many values across `args` satisfy `keep`, with ranges flattened.
    fn count_matching(&mut self, args: &[Expr], keep: impl Fn(&CellValue) -> bool) -> usize {
        let mut n = 0usize;
        for a in args {
            let hits = self.collect_values(a).iter().filter(|v| keep(v)).count();
            n = n.saturating_add(hits);
        }
        n
    }

    /// The SUM/AVERAGE/COUNT family, including the conditional `*IF`/`*IFS`
    /// variants.
    fn eval_aggregate(&mut self, name: &str, args: &[Expr]) -> Option<CellValue> {
        let value = match name {
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
                    CellValue::Number(ns.iter().sum::<f64>() / count_number(ns.len()))
                }
                Ok(_) => CellValue::Error(FormulaError::Div0),
                Err(e) => e,
            },
            "MEDIAN" => match self.numeric_args(args) {
                Ok(mut ns) => match median(&mut ns) {
                    Some(m) => CellValue::Number(m),
                    None => CellValue::Error(FormulaError::Num),
                },
                Err(e) => e,
            },
            "MIN" => match self.numeric_args(args) {
                Ok(ns) if ns.is_empty() => CellValue::Number(0.0),
                Ok(ns) => CellValue::Number(ns.iter().copied().fold(f64::INFINITY, f64::min)),
                Err(e) => e,
            },
            "MAX" => match self.numeric_args(args) {
                Ok(ns) if ns.is_empty() => CellValue::Number(0.0),
                Ok(ns) => CellValue::Number(ns.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
                Err(e) => e,
            },
            // Count of numeric cells.
            "COUNT" => CellValue::Number(count_number(
                self.count_matching(args, |v| matches!(v, CellValue::Number(_))),
            )),
            // Count of non-empty cells.
            "COUNTA" => {
                CellValue::Number(count_number(self.count_matching(args, |v| !v.is_empty())))
            }
            "COUNTBLANK" => {
                CellValue::Number(count_number(self.count_matching(args, CellValue::is_empty)))
            }
            "SUMIF" => self.sumif(args),
            "COUNTIF" => self.countif(args),
            "AVERAGEIF" => self.averageif(args),
            "COUNTIFS" => match self.criteria_mask(args) {
                Ok(mask) => CellValue::Number(count_number(mask.iter().filter(|b| **b).count())),
                Err(e) => e,
            },
            "SUMIFS" => {
                let Some((values, criteria)) = args.split_first() else {
                    return Some(CellValue::Error(FormulaError::Error));
                };
                let vals = self.collect_values(values);
                match self.criteria_mask(criteria) {
                    Ok(mask) => {
                        let mut total = 0.0;
                        for (v, keep) in vals.iter().zip(mask.iter()) {
                            if *keep && let Some(n) = v.as_number() {
                                total += n;
                            }
                        }
                        CellValue::Number(total)
                    }
                    Err(e) => e,
                }
            }
            "AVERAGEIFS" => {
                let Some((values, criteria)) = args.split_first() else {
                    return Some(CellValue::Error(FormulaError::Error));
                };
                let vals = self.collect_values(values);
                match self.criteria_mask(criteria) {
                    Ok(mask) => {
                        let mut sum = 0.0;
                        let mut cnt = 0usize;
                        for (v, keep) in vals.iter().zip(mask.iter()) {
                            if *keep && let Some(n) = v.as_number() {
                                sum += n;
                                cnt = cnt.saturating_add(1);
                            }
                        }
                        if cnt == 0 {
                            CellValue::Error(FormulaError::Div0)
                        } else {
                            CellValue::Number(sum / count_number(cnt))
                        }
                    }
                    Err(e) => e,
                }
            }
            _ => return None,
        };
        Some(value)
    }

    /// IF/IFS/SWITCH and the boolean connectives.
    fn eval_logical(&mut self, name: &str, args: &[Expr]) -> Option<CellValue> {
        let value = match name {
            "IF" => self.eval_if(args),
            "IFS" => self.eval_ifs(args),
            "IFERROR" => self.eval_iferror(args),
            "IFNA" => self.eval_ifna(args),
            "SWITCH" => self.eval_switch(args),
            "TRUE" => CellValue::Bool(true),
            "FALSE" => CellValue::Bool(false),
            "XOR" => match self.flatten_bools(args) {
                Ok(bs) => CellValue::Bool(bs.iter().filter(|b| **b).count() % 2 == 1),
                Err(e) => e,
            },
            "AND" => match self.flatten_bools(args) {
                Ok(bs) => CellValue::Bool(bs.into_iter().all(|b| b)),
                Err(e) => e,
            },
            "OR" => match self.flatten_bools(args) {
                Ok(bs) => CellValue::Bool(bs.into_iter().any(|b| b)),
                Err(e) => e,
            },
            "NOT" => self.eval_not(args),
            _ => return None,
        };
        Some(value)
    }

    fn eval_if(&mut self, args: &[Expr]) -> CellValue {
        let (Some(cond_expr), Some(then_expr)) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        let cond = self.eval_expr(cond_expr);
        if cond.is_error() {
            return cond;
        }
        if cond_truthy(&cond) {
            self.eval_expr(then_expr)
        } else if let Some(else_expr) = args.get(2) {
            self.eval_expr(else_expr)
        } else {
            CellValue::Bool(false)
        }
    }

    fn eval_ifs(&mut self, args: &[Expr]) -> CellValue {
        let mut chosen = CellValue::Error(FormulaError::Na);
        for pair in args.chunks_exact(2) {
            let (Some(cond_expr), Some(then_expr)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            let cond = self.eval_expr(cond_expr);
            if cond.is_error() {
                return cond;
            }
            if cond_truthy(&cond) {
                chosen = self.eval_expr(then_expr);
                break;
            }
        }
        chosen
    }

    fn eval_iferror(&mut self, args: &[Expr]) -> CellValue {
        let (Some(probe), Some(fallback)) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        let v = self.eval_expr(probe);
        if v.is_error() {
            self.eval_expr(fallback)
        } else {
            v
        }
    }

    fn eval_ifna(&mut self, args: &[Expr]) -> CellValue {
        let (Some(probe), Some(fallback)) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        let v = self.eval_expr(probe);
        if matches!(&v, CellValue::Error(FormulaError::Na)) {
            self.eval_expr(fallback)
        } else {
            v
        }
    }

    fn eval_switch(&mut self, args: &[Expr]) -> CellValue {
        let Some((target_expr, rest)) = args.split_first() else {
            return CellValue::Error(FormulaError::Error);
        };
        let target = self.eval_expr(target_expr);
        let mut cases = rest.chunks_exact(2);
        let mut chosen: Option<&Expr> = None;
        for pair in cases.by_ref() {
            let (Some(case_expr), Some(value_expr)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            let case = self.eval_expr(case_expr);
            if values_equal(&case, &target) {
                chosen = Some(value_expr);
                break;
            }
        }
        // A lone trailing argument is the default.
        match chosen.or_else(|| cases.remainder().first()) {
            Some(e) => self.eval_expr(e),
            None => CellValue::Error(FormulaError::Na),
        }
    }

    fn eval_not(&mut self, args: &[Expr]) -> CellValue {
        let [only] = args else {
            return CellValue::Error(FormulaError::Error);
        };
        match self.eval_expr(only).as_number() {
            Some(n) => CellValue::Bool(n == 0.0),
            None => CellValue::Error(FormulaError::Value),
        }
    }

    /// The scalar maths functions.
    fn eval_math(&mut self, name: &str, args: &[Expr]) -> Option<CellValue> {
        let value = match name {
            "ABS" => self.unary_num(args, f64::abs),
            "SQRT" => self.unary_num(args, f64::sqrt),
            "INT" | "FLOOR" => self.unary_num(args, f64::floor),
            "SIGN" => self.unary_num(args, f64::signum),
            "EXP" => self.unary_num(args, f64::exp),
            "LN" => self.unary_num(args, f64::ln),
            "LOG10" => self.unary_num(args, f64::log10),
            "LOG" => {
                let v = self.eval_expr(args.first().unwrap_or(&Expr::Num(0.0)));
                if v.is_error() {
                    return Some(v);
                }
                let Some(n) = v.as_number() else {
                    return Some(CellValue::Error(FormulaError::Value));
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
            "CEILING" => self.unary_num(args, f64::ceil),
            _ => return None,
        };
        Some(value)
    }

    /// The text functions.
    fn eval_text(&mut self, name: &str, args: &[Expr]) -> Option<CellValue> {
        let value = match name {
            "LEN" => {
                let text = self
                    .eval_expr(args.first().unwrap_or(&Expr::Str(String::new())))
                    .to_plain_string();
                CellValue::Number(count_number(text.chars().count()))
            }
            "UPPER" => self.unary_text(args, str::to_uppercase),
            "LOWER" => self.unary_text(args, str::to_lowercase),
            "TRIM" => self.unary_text(args, |s| s.trim().to_string()),
            "PROPER" => self.unary_text(args, proper_case),
            "LEFT" => self.substr_fn(args, true),
            "RIGHT" => self.substr_fn(args, false),
            "MID" => self.mid_fn(args),
            "CONCAT" | "CONCATENATE" => {
                let mut s = String::new();
                for a in args {
                    for v in self.collect_values(a) {
                        if v.is_error() {
                            return Some(v);
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
            "REPT" => {
                let s = self
                    .eval_expr(args.first().unwrap_or(&Expr::Str(String::new())))
                    .to_plain_string();
                let count = args
                    .get(1)
                    .and_then(|a| self.eval_expr(a).as_number())
                    .map_or(0, |n| number_to_index(n, 10_000));
                CellValue::Text(s.repeat(count))
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
                let (Some(left), Some(right)) = (args.first(), args.get(1)) else {
                    return Some(CellValue::Error(FormulaError::Error));
                };
                let a = self.eval_expr(left).to_plain_string();
                let b = self.eval_expr(right).to_plain_string();
                CellValue::Bool(a == b)
            }
            "CHAR" => {
                let code = args
                    .first()
                    .and_then(|a| self.eval_expr(a).as_number())
                    .map_or(0, |n| u32::try_from(number_to_int(n)).unwrap_or(0));
                match char::from_u32(code) {
                    Some(c) => CellValue::Text(c.to_string()),
                    None => CellValue::Error(FormulaError::Value),
                }
            }
            "CODE" => {
                let s = self
                    .eval_expr(args.first().unwrap_or(&Expr::Str(String::new())))
                    .to_plain_string();
                match s.chars().next() {
                    Some(c) => CellValue::Number(f64::from(u32::from(c))),
                    None => CellValue::Error(FormulaError::Value),
                }
            }
            "TEXTJOIN" => self.textjoin(args),
            "TEXT" => {
                let (Some(value_expr), Some(code_expr)) = (args.first(), args.get(1)) else {
                    return Some(CellValue::Error(FormulaError::Error));
                };
                let v = self.eval_expr(value_expr);
                if v.is_error() {
                    return Some(v);
                }
                let code = self.eval_expr(code_expr).to_plain_string();
                match v.as_number() {
                    Some(n) => CellValue::Text(apply_text_code(n, &code)),
                    None => CellValue::Text(v.to_plain_string()),
                }
            }
            _ => return None,
        };
        Some(value)
    }

    /// The lookup functions.
    fn eval_lookup(&mut self, name: &str, args: &[Expr]) -> Option<CellValue> {
        let value = match name {
            "VLOOKUP" => self.vlookup(args),
            "HLOOKUP" => self.hlookup(args),
            "INDEX" => self.index_fn(args),
            "MATCH" => self.match_fn(args),
            "CHOOSE" => {
                let index = args
                    .first()
                    .and_then(|a| self.eval_expr(a).as_number())
                    .map_or(0, number_to_int);
                // Argument 0 is the index itself, so a valid pick starts at 1.
                let picked = usize::try_from(index)
                    .ok()
                    .filter(|i| *i >= 1)
                    .and_then(|i| args.get(i));
                match picked {
                    Some(e) => self.eval_expr(e),
                    None => CellValue::Error(FormulaError::Value),
                }
            }
            _ => return None,
        };
        Some(value)
    }

    /// The date and time functions.
    fn eval_datetime(&mut self, name: &str, args: &[Expr]) -> Option<CellValue> {
        let value = match name {
            "DATE" => {
                let (Some(y_expr), Some(m_expr), Some(d_expr)) =
                    (args.first(), args.get(1), args.get(2))
                else {
                    return Some(CellValue::Error(FormulaError::Error));
                };
                let year_arg = self.eval_expr(y_expr).as_number();
                let month_arg = self.eval_expr(m_expr).as_number();
                let day_arg = self.eval_expr(d_expr).as_number();
                let (Some(y), Some(m), Some(d)) = (year_arg, month_arg, day_arg) else {
                    return Some(CellValue::Error(FormulaError::Value));
                };
                // Normalize out-of-range months/days the way Excel does.
                let month_index = number_to_int(m).saturating_sub(1);
                let year = number_to_int(y).saturating_add(month_index.div_euclid(12));
                let month = u32::try_from(month_index.rem_euclid(12))
                    .unwrap_or(0)
                    .saturating_add(1);
                let day_offset = int_number(number_to_int(d).saturating_sub(1));
                CellValue::Number(crate::model::date_to_serial(year, month, 1) + day_offset)
            }
            "TIME" => {
                let Some(h_expr) = args.first() else {
                    return Some(CellValue::Error(FormulaError::Error));
                };
                let h = self.eval_expr(h_expr).as_number().unwrap_or(0.0);
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
            "YEAR" => self.date_part(args, |(y, _, _)| int_number(y)),
            "MONTH" => self.date_part(args, |(_, m, _)| f64::from(m)),
            "DAY" => self.date_part(args, |(_, _, d)| f64::from(d)),
            "HOUR" => self.time_part(args, TimeComponent::Hour),
            "MINUTE" => self.time_part(args, TimeComponent::Minute),
            "SECOND" => self.time_part(args, TimeComponent::Second),
            "WEEKDAY" => {
                let v = self.eval_expr(args.first().unwrap_or(&Expr::Num(0.0)));
                if v.is_error() {
                    return Some(v);
                }
                let Some(n) = v.as_number() else {
                    return Some(CellValue::Error(FormulaError::Value));
                };
                let weekday = i64::from(crate::model::weekday_from_serial(n)); // 1=Sun..7=Sat
                let scheme = args
                    .get(1)
                    .and_then(|a| self.eval_expr(a).as_number())
                    .map_or(1, number_to_int);
                let shifted = weekday.saturating_add(5).rem_euclid(7);
                let out = match scheme {
                    2 => shifted.saturating_add(1), // Mon=1..Sun=7
                    3 => shifted,                   // Mon=0..Sun=6
                    _ => weekday,                   // Sun=1..Sat=7
                };
                CellValue::Number(int_number(out))
            }
            _ => return None,
        };
        Some(value)
    }

    fn flatten_bools(&mut self, args: &[Expr]) -> Result<Vec<bool>, CellValue> {
        let mut out = Vec::new();
        for a in args {
            for v in self.collect_values(a) {
                if v.is_error() {
                    return Err(v);
                }
                // Non-numeric values are skipped.
                if let Some(n) = v.as_number() {
                    out.push(n != 0.0);
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

    fn binary_num(&mut self, args: &[Expr], op: impl Fn(f64, f64) -> Option<f64>) -> CellValue {
        let [left, right] = args else {
            return CellValue::Error(FormulaError::Error);
        };
        let lv = self.eval_expr(left);
        let rv = self.eval_expr(right);
        if lv.is_error() {
            return lv;
        }
        if rv.is_error() {
            return rv;
        }
        match (lv.as_number(), rv.as_number()) {
            (Some(ln), Some(rn)) => match op(ln, rn) {
                Some(result) => CellValue::Number(result),
                None => CellValue::Error(FormulaError::Div0),
            },
            _ => CellValue::Error(FormulaError::Value),
        }
    }

    fn round_fn(&mut self, args: &[Expr], mode: RoundMode) -> CellValue {
        let Some(value_expr) = args.first() else {
            return CellValue::Error(FormulaError::Error);
        };
        let v = self.eval_expr(value_expr);
        if v.is_error() {
            return v;
        }
        let Some(n) = v.as_number() else {
            return CellValue::Error(FormulaError::Value);
        };
        // Past ±308 the scale factor is already 0 or infinite, so clamping
        // there loses nothing and keeps the exponent in range.
        let digits = args.get(1).map_or(0, |a| {
            let scale = self.eval_expr(a).as_number().unwrap_or(0.0);
            i32::try_from(number_to_int(scale).clamp(-308, 308)).unwrap_or(0)
        });
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
        let Some(text_expr) = args.first() else {
            return CellValue::Error(FormulaError::Error);
        };
        let chars: Vec<char> = self
            .eval_expr(text_expr)
            .to_plain_string()
            .chars()
            .collect();
        let take = args.get(1).map_or(1, |a| {
            let n = self.eval_expr(a).as_number().unwrap_or(1.0);
            number_to_index(n, chars.len())
        });
        let slice: String = if from_left {
            chars.iter().take(take).collect()
        } else {
            chars
                .iter()
                .skip(chars.len().saturating_sub(take))
                .collect()
        };
        CellValue::Text(slice)
    }

    fn mid_fn(&mut self, args: &[Expr]) -> CellValue {
        let (Some(text_expr), Some(start_expr), Some(len_expr)) =
            (args.first(), args.get(1), args.get(2))
        else {
            return CellValue::Error(FormulaError::Error);
        };
        let chars: Vec<char> = self
            .eval_expr(text_expr)
            .to_plain_string()
            .chars()
            .collect();
        // MID counts from 1, so the 1-based start becomes a 0-based skip.
        let start = number_to_index(
            self.eval_expr(start_expr)
                .as_number()
                .unwrap_or(1.0)
                .max(1.0),
            chars.len(),
        )
        .saturating_sub(1);
        let len = number_to_index(
            self.eval_expr(len_expr).as_number().unwrap_or(0.0),
            chars.len(),
        );
        CellValue::Text(chars.iter().skip(start).take(len).collect())
    }

    /// SUMIF(range, criterion, [`sum_range`]).
    fn sumif(&mut self, args: &[Expr]) -> CellValue {
        let (Some(range_expr), Some(crit_expr)) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        let probe = self.collect_values(range_expr);
        let crit = self.eval_expr(crit_expr);
        let sum_vals = match args.get(2) {
            Some(a) => self.collect_values(a),
            None => probe.clone(),
        };
        let mut total = 0.0;
        for (v, summand) in probe.iter().zip(sum_vals.iter()) {
            if criterion_matches(v, &crit)
                && let Some(n) = summand.as_number()
            {
                total += n;
            }
        }
        CellValue::Number(total)
    }

    /// COUNTIF(range, criterion).
    fn countif(&mut self, args: &[Expr]) -> CellValue {
        let (Some(range_expr), Some(crit_expr)) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        let probe = self.collect_values(range_expr);
        let crit = self.eval_expr(crit_expr);
        let n = probe.iter().filter(|v| criterion_matches(v, &crit)).count();
        CellValue::Number(count_number(n))
    }

    /// AVERAGEIF(range, criterion, [`average_range`]).
    fn averageif(&mut self, args: &[Expr]) -> CellValue {
        let (Some(range_expr), Some(crit_expr)) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        let probe = self.collect_values(range_expr);
        let crit = self.eval_expr(crit_expr);
        let vals = match args.get(2) {
            Some(a) => self.collect_values(a),
            None => probe.clone(),
        };
        let mut sum = 0.0;
        let mut cnt = 0usize;
        for (v, sample) in probe.iter().zip(vals.iter()) {
            if criterion_matches(v, &crit)
                && let Some(n) = sample.as_number()
            {
                sum += n;
                cnt = cnt.saturating_add(1);
            }
        }
        if cnt == 0 {
            CellValue::Error(FormulaError::Div0)
        } else {
            CellValue::Number(sum / count_number(cnt))
        }
    }

    /// Build the per-cell keep mask for the `*IFS` family from a flat list of
    /// `range, criterion` pairs. Every criterion must hold for a position to be
    /// kept; ranges shorter than the first simply stop contributing.
    fn criteria_mask(&mut self, pairs: &[Expr]) -> Result<Vec<bool>, CellValue> {
        let mut mask: Option<Vec<bool>> = None;
        for pair in pairs.chunks_exact(2) {
            let (Some(range_expr), Some(crit_expr)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            let probe = self.collect_values(range_expr);
            let crit = self.eval_expr(crit_expr);
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
        }
        Ok(mask.unwrap_or_default())
    }

    /// SUBSTITUTE(text, old, new, [instance]) — replace all occurrences, or
    /// only the `instance`-th (1-based) when given.
    fn substitute(&mut self, args: &[Expr]) -> CellValue {
        let (Some(text_expr), Some(old_expr), Some(new_expr)) =
            (args.first(), args.get(1), args.get(2))
        else {
            return CellValue::Error(FormulaError::Error);
        };
        let text = self.eval_expr(text_expr).to_plain_string();
        let old = self.eval_expr(old_expr).to_plain_string();
        let new = self.eval_expr(new_expr).to_plain_string();
        if old.is_empty() {
            return CellValue::Text(text);
        }
        let instance = args
            .get(3)
            .and_then(|a| self.eval_expr(a).as_number())
            .map(|n| number_to_index(n, usize::MAX));
        let out = match instance {
            None => text.replace(&old, &new),
            Some(target) if target >= 1 => {
                let mut result = String::new();
                let mut rest = text.as_str();
                let mut count = 0usize;
                let mut replaced = false;
                while let Some(pos) = rest.find(&old) {
                    let after = pos.saturating_add(old.len());
                    let (Some(head), Some(tail)) = (rest.get(..pos), rest.get(after..)) else {
                        break;
                    };
                    count = count.saturating_add(1);
                    result.push_str(head);
                    if count == target {
                        result.push_str(&new);
                        result.push_str(tail);
                        replaced = true;
                        break;
                    }
                    result.push_str(&old);
                    rest = tail;
                }
                if !replaced {
                    result.push_str(rest);
                }
                result
            }
            _ => text,
        };
        CellValue::Text(out)
    }

    /// `REPLACE(old_text`, start, `num_chars`, `new_text`) — positional replacement.
    fn replace_fn(&mut self, args: &[Expr]) -> CellValue {
        let (Some(text_expr), Some(start_expr), Some(num_expr), Some(new_expr)) =
            (args.first(), args.get(1), args.get(2), args.get(3))
        else {
            return CellValue::Error(FormulaError::Error);
        };
        let chars: Vec<char> = self
            .eval_expr(text_expr)
            .to_plain_string()
            .chars()
            .collect();
        // REPLACE counts from 1, so the 1-based start becomes a 0-based skip.
        let start = number_to_index(
            self.eval_expr(start_expr)
                .as_number()
                .unwrap_or(1.0)
                .max(1.0),
            chars.len(),
        )
        .saturating_sub(1);
        let num = number_to_index(
            self.eval_expr(num_expr).as_number().unwrap_or(0.0),
            chars.len(),
        );
        let new = self.eval_expr(new_expr).to_plain_string();
        let mut out: String = chars.iter().take(start).collect();
        out.push_str(&new);
        out.extend(chars.iter().skip(start.saturating_add(num)));
        CellValue::Text(out)
    }

    /// FIND/SEARCH(needle, haystack, [start]) — 1-based position of `needle`,
    /// case-sensitive for FIND and case-insensitive for SEARCH.
    fn find_fn(&mut self, args: &[Expr], case_sensitive: bool) -> CellValue {
        let (Some(needle_expr), Some(hay_expr)) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        let needle = self.eval_expr(needle_expr).to_plain_string();
        let hay = self.eval_expr(hay_expr).to_plain_string();
        // The optional start is 1-based; below 1 it means "from the beginning".
        let start = args
            .get(2)
            .and_then(|a| self.eval_expr(a).as_number())
            .map_or(1, |n| number_to_index(n, usize::MAX).max(1))
            .saturating_sub(1);
        let (haystack, pattern) = if case_sensitive {
            (hay, needle)
        } else {
            (hay.to_lowercase(), needle.to_lowercase())
        };
        let hay_chars: Vec<char> = haystack.chars().collect();
        if start > hay_chars.len() {
            return CellValue::Error(FormulaError::Value);
        }
        let tail: String = hay_chars.iter().skip(start).collect();
        match tail.find(&pattern) {
            Some(byte_pos) => {
                let char_off = tail.get(..byte_pos).map_or(0, |p| p.chars().count());
                CellValue::Number(count_number(
                    start.saturating_add(char_off).saturating_add(1),
                ))
            }
            None => CellValue::Error(FormulaError::Value),
        }
    }

    /// TEXTJOIN(delimiter, `ignore_empty`, values…).
    fn textjoin(&mut self, args: &[Expr]) -> CellValue {
        let (Some(delim_expr), Some(flag_expr)) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        let Some(values) = args.get(2..).filter(|rest| !rest.is_empty()) else {
            return CellValue::Error(FormulaError::Error);
        };
        let delim = self.eval_expr(delim_expr).to_plain_string();
        let ignore_empty = cond_truthy(&self.eval_expr(flag_expr));
        let mut parts = Vec::new();
        for a in values {
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
        self.lookup(args, Axis::Vertical)
    }

    /// HLOOKUP(lookup, table, `row_index`, [`range_lookup`]).
    fn hlookup(&mut self, args: &[Expr]) -> CellValue {
        self.lookup(args, Axis::Horizontal)
    }

    /// Shared body for VLOOKUP/HLOOKUP: scan the leading column (or row) of the
    /// table for `key`, then read the cell `index - 1` steps across the other
    /// axis.
    fn lookup(&mut self, args: &[Expr], axis: Axis) -> CellValue {
        let (Some(key_expr), Some(table_expr), Some(index_expr)) =
            (args.first(), args.get(1), args.get(2))
        else {
            return CellValue::Error(FormulaError::Error);
        };
        let key = self.eval_expr(key_expr);
        if key.is_error() {
            return key;
        }
        let Some((tl, br)) = range_bounds(table_expr) else {
            return CellValue::Error(FormulaError::Ref);
        };
        let Some(index) = self.eval_expr(index_expr).as_number().map(number_to_int) else {
            return CellValue::Error(FormulaError::Value);
        };
        if index < 1 {
            return CellValue::Error(FormulaError::Value);
        }
        // `along` runs down the probe axis; `across` picks the answer column/row.
        let (along_from, along_to, across_first, across_last) = match axis {
            Axis::Vertical => (tl.1, br.1, tl.0, br.0),
            Axis::Horizontal => (tl.0, br.0, tl.1, br.1),
        };
        let target = i64::from(across_first)
            .saturating_add(index)
            .saturating_sub(1);
        if target > i64::from(across_last) {
            return CellValue::Error(FormulaError::Ref);
        }
        let target = u32::try_from(target).unwrap_or(across_first);
        let approx = args
            .get(3)
            .is_none_or(|a| self.eval_expr(a).as_number() != Some(0.0));
        let mut best: Option<u32> = None;
        for along in along_from..=along_to {
            let probe = self.eval_cell(axis.addr(along, across_first));
            if approx {
                if lte(&probe, &key) {
                    best = Some(along);
                } else {
                    break;
                }
            } else if values_equal(&probe, &key) {
                return self.eval_cell(axis.addr(along, target));
            }
        }
        match best {
            Some(along) => self.eval_cell(axis.addr(along, target)),
            None => CellValue::Error(FormulaError::Na),
        }
    }

    /// INDEX(range, `row_num`, [`col_num`]). A single-row or single-column range
    /// takes one index; a rectangle takes both.
    fn index_fn(&mut self, args: &[Expr]) -> CellValue {
        let Some((tl, br)) = args.first().and_then(range_bounds) else {
            return CellValue::Error(FormulaError::Ref);
        };
        let single_row = br.1 == tl.1;
        let first_index = args
            .get(1)
            .and_then(|a| self.eval_expr(a).as_number())
            .map_or(0, number_to_int);
        let second_index = args
            .get(2)
            .and_then(|a| self.eval_expr(a).as_number())
            .map(number_to_int);
        // With one index, a single-row range indexes columns; anything else
        // indexes rows.
        let (row_i, col_i) = match second_index {
            Some(col) => (first_index, col),
            None if single_row => (1, first_index),
            None => (first_index, 1),
        };
        if row_i < 1 || col_i < 1 {
            return CellValue::Error(FormulaError::Value);
        }
        let col = i64::from(tl.0).saturating_add(col_i).saturating_sub(1);
        let row = i64::from(tl.1).saturating_add(row_i).saturating_sub(1);
        if col > i64::from(br.0) || row > i64::from(br.1) {
            return CellValue::Error(FormulaError::Ref);
        }
        let (Ok(col), Ok(row)) = (u32::try_from(col), u32::try_from(row)) else {
            return CellValue::Error(FormulaError::Ref);
        };
        self.eval_cell((col, row))
    }

    /// MATCH(lookup, range, [`match_type`]). `0` exact, `1` largest ≤ (ascending,
    /// default), `-1` smallest ≥ (descending).
    fn match_fn(&mut self, args: &[Expr]) -> CellValue {
        let (Some(key_expr), Some(range_expr)) = (args.first(), args.get(1)) else {
            return CellValue::Error(FormulaError::Error);
        };
        let key = self.eval_expr(key_expr);
        let Some((tl, br)) = range_bounds(range_expr) else {
            return CellValue::Error(FormulaError::Na);
        };
        let match_type = args
            .get(2)
            .and_then(|a| self.eval_expr(a).as_number())
            .map_or(1, number_to_int);
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
                        return CellValue::Number(count_number(i.saturating_add(1)));
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
            Some(i) => CellValue::Number(count_number(i.saturating_add(1))),
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

    /// Shared body for HOUR/MINUTE/SECOND.
    fn time_part(&mut self, args: &[Expr], which: TimeComponent) -> CellValue {
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
        let (hours, minutes, seconds) = time_parts(n);
        CellValue::Number(int_number(match which {
            TimeComponent::Hour => hours,
            TimeComponent::Minute => minutes,
            TimeComponent::Second => seconds,
        }))
    }
}

#[derive(Clone, Copy)]
enum RoundMode {
    Nearest,
    Down,
    Up,
}

/// Which way a lookup scans its table.
#[derive(Clone, Copy)]
enum Axis {
    /// VLOOKUP: probe down the leading column, answer from a column.
    Vertical,
    /// HLOOKUP: probe across the leading row, answer from a row.
    Horizontal,
}

impl Axis {
    /// The address `along` steps down the probe axis and `across` steps along
    /// the other one.
    fn addr(self, along: u32, across: u32) -> Addr {
        match self {
            Self::Vertical => (across, along),
            Self::Horizontal => (along, across),
        }
    }
}

/// Compare two evaluated operands under a comparison operator: numerically
/// when both coerce to numbers, otherwise as text.
fn eval_compare(op: BinOp, lv: &CellValue, rv: &CellValue) -> CellValue {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let ord = match (lv.as_number(), rv.as_number()) {
        (Some(a), Some(b)) => a.partial_cmp(&b),
        _ => Some(lv.to_plain_string().cmp(&rv.to_plain_string())),
    };
    let Some(ord) = ord else {
        // A NaN operand orders against nothing.
        return CellValue::Error(FormulaError::Value);
    };
    let res = match op {
        BinOp::Eq => ord == Equal,
        BinOp::Ne => ord != Equal,
        BinOp::Lt => ord == Less,
        BinOp::Gt => ord == Greater,
        BinOp::Le => ord != Greater,
        BinOp::Ge => ord != Less,
        // Only the comparison operators reach here.
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow | BinOp::Concat => {
            return CellValue::Error(FormulaError::Value);
        }
    };
    CellValue::Bool(res)
}

/// Whether a probe value satisfies a COUNTIF/SUMIF criterion. Supports a
/// bare value (equality) and the string operators `">5"`, `"<=3"`,
/// `"<>x"`, `">=0"`.
fn criterion_matches(probe: &CellValue, crit: &CellValue) -> bool {
    if let CellValue::Text(text) = crit {
        let text = text.trim();
        for op in ["<=", ">=", "<>", "<", ">", "="] {
            if let Some(rest) = text.strip_prefix(op) {
                if let Ok(threshold) = rest.trim().parse::<f64>() {
                    let Some(value) = probe.as_number() else {
                        return false;
                    };
                    return match op {
                        "<=" => value <= threshold,
                        ">=" => value >= threshold,
                        "<>" => !numbers_equal(value, threshold),
                        "<" => value < threshold,
                        ">" => value > threshold,
                        "=" => numbers_equal(value, threshold),
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
        (Some(a), Some(b)) => numbers_equal(a, b),
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
        (Some(x), Some(y)) => numbers_equal(x, y),
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
        CellValue::Empty | CellValue::Error(_) => false,
        CellValue::Text(t) => match t.to_ascii_uppercase().as_str() {
            "TRUE" => true,
            "FALSE" => false,
            _ => !t.is_empty(),
        },
    }
}

/// The `(hours, minutes, seconds)` of a serial's fractional day.
fn time_parts(serial: f64) -> (i64, i64, i64) {
    let seconds_of_day = number_to_int((serial.fract().abs() * 86400.0).round());
    (
        seconds_of_day.div_euclid(3600).rem_euclid(24),
        seconds_of_day.rem_euclid(3600).div_euclid(60),
        seconds_of_day.rem_euclid(60),
    )
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

/// A 1-based month number as a 0-based index into a twelve-entry name table.
fn month_index(m: u32) -> usize {
    usize::try_from(m.clamp(1, 12))
        .unwrap_or(1)
        .saturating_sub(1)
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
    M.get(month_index(m)).copied().unwrap_or("January")
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
    let decimals = code.split('.').nth(1).map_or(0, |frac| {
        frac.chars().filter(|c| *c == '0' || *c == '#').count()
    });
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
    let (year, month, day) = crate::model::serial_to_ymd(serial);
    let (hours, minutes, seconds) = time_parts(serial);
    let lowered: Vec<char> = code.to_lowercase().chars().collect();
    let mut scan = Scan::new(&lowered);
    let has_ampm = code.to_lowercase().contains("am/pm");
    let shown_hour = if has_ampm {
        match hours.rem_euclid(12) {
            0 => 12,
            h => h,
        }
    } else {
        hours
    };
    let mut out = String::new();
    // "mm"/"m" mean minutes only when we've just passed an hour token / colon.
    let mut time_ctx = false;
    while let Some(ch) = scan.peek() {
        if scan.starts_with("am/pm") {
            out.push_str(if hours < 12 { "AM" } else { "PM" });
            scan.advance(5);
        } else if scan.starts_with("yyyy") {
            let _ = write!(out, "{year:04}");
            scan.advance(4);
            time_ctx = false;
        } else if scan.starts_with("yy") {
            let _ = write!(out, "{:02}", year.rem_euclid(100));
            scan.advance(2);
            time_ctx = false;
        } else if scan.starts_with("mmmm") {
            out.push_str(full_month(month));
            scan.advance(4);
        } else if scan.starts_with("mmm") {
            out.push_str(month_abbrev_local(month));
            scan.advance(3);
        } else if scan.starts_with("hh") {
            let _ = write!(out, "{shown_hour:02}");
            scan.advance(2);
            time_ctx = true;
        } else if scan.starts_with("h") {
            let _ = write!(out, "{shown_hour}");
            scan.advance(1);
            time_ctx = true;
        } else if scan.starts_with("ss") {
            let _ = write!(out, "{seconds:02}");
            scan.advance(2);
        } else if scan.starts_with("dd") {
            let _ = write!(out, "{day:02}");
            scan.advance(2);
            time_ctx = false;
        } else if scan.starts_with("d") {
            let _ = write!(out, "{day}");
            scan.advance(1);
            time_ctx = false;
        } else if scan.starts_with("mm") {
            if time_ctx {
                let _ = write!(out, "{minutes:02}");
                time_ctx = false;
            } else {
                let _ = write!(out, "{month:02}");
            }
            scan.advance(2);
        } else if scan.starts_with("m") {
            if time_ctx {
                let _ = write!(out, "{minutes}");
                time_ctx = false;
            } else {
                let _ = write!(out, "{month}");
            }
            scan.advance(1);
        } else {
            if ch == ':' {
                time_ctx = true;
            }
            out.push(ch);
            scan.advance(1);
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
    M.get(month_index(m)).copied().unwrap_or("Jan")
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
        && let Ok(n) = p.trim().parse::<f64>()
    {
        return CellValue::Number(n / 100.0);
    }
    match trimmed.to_uppercase().as_str() {
        "TRUE" => CellValue::Bool(true),
        "FALSE" => CellValue::Bool(false),
        _ => CellValue::Text(raw.to_string()),
    }
}

/// Convenience: evaluate a whole sheet's raw inputs into a value map.
pub fn recalculate<S: BuildHasher>(raw: &HashMap<Addr, String, S>) -> HashMap<Addr, CellValue> {
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
    Currency(tilo_locale::Currency),
    /// A percentage value.
    Percent,
    /// A date serial.
    Date(tilo_locale::DateStyle),
    /// A time/duration serial.
    Time(ui::clock::TimeStyle),
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
pub fn infer_result_format<S: BuildHasher>(
    body: &str,
    formats: &HashMap<Addr, CellFormat, S>,
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
fn unit_of<S: BuildHasher>(expr: &Expr, formats: &HashMap<Addr, CellFormat, S>) -> Unit {
    match expr {
        Expr::Num(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Range(_, _) => Unit::Scalar,
        Expr::Ref(addr) => formats
            .get(addr)
            .map_or(Unit::Scalar, |f| unit_of_format(*f)),
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
    use BinOp::{Add, Concat, Div, Eq, Ge, Gt, Le, Lt, Mul, Ne, Pow, Sub};
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
fn unit_of_call<S: BuildHasher>(
    name: &str,
    args: &[Expr],
    formats: &HashMap<Addr, CellFormat, S>,
) -> Unit {
    match name {
        // Sum-family + selection functions preserve a uniform unit.
        "SUM" | "AVERAGE" | "MIN" | "MAX" | "MEDIAN" | "PRODUCT" | "ROUND" | "ROUNDUP"
        | "ROUNDDOWN" | "ABS" | "FLOOR" | "CEILING" | "INT" => {
            let mut acc: Option<Unit> = None;
            for a in args {
                for u in arg_units(a, formats) {
                    acc = Some(match acc {
                        None | Some(Unit::Scalar) => u,
                        Some(prev) if prev == u || u == Unit::Scalar => prev,
                        Some(_) => Unit::Mixed,
                    });
                }
            }
            acc.unwrap_or(Unit::Scalar)
        }
        // Date/time builders render as dates/times so a bare `=TODAY()` shows
        // a date rather than its underlying serial number.
        "DATE" | "TODAY" | "NOW" => Unit::Date(tilo_locale::DateStyle::Iso),
        "TIME" => Unit::Time(ui::clock::TimeStyle::H24),
        // Counting / text / logical / lookup functions are unitless.
        _ => Unit::Scalar,
    }
}

/// Units contributed by a single argument — a range contributes the unit of
/// each cell it spans.
fn arg_units<S: BuildHasher>(arg: &Expr, formats: &HashMap<Addr, CellFormat, S>) -> Vec<Unit> {
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

    /// Precedence, references/SUM, IF/comparisons, div-zero/cycles and Excel error codes.
    #[test]
    fn arithmetic_references_conditionals_and_errors() {
        // Case `arithmetic_and_precedence`.
        {
            let m = run(&[("A1", "=2+3*4"), ("A2", "=(2+3)*4"), ("A3", "=2^3^2")]);
            assert_eq!(val(&m, "A1"), CellValue::Number(14.0));
            assert_eq!(val(&m, "A2"), CellValue::Number(20.0));
            // right-assoc power: 2^(3^2) = 512
            assert_eq!(val(&m, "A3"), CellValue::Number(512.0));
        }

        // Case `cell_refs_and_sum_range`.
        {
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

        // Case `if_and_comparisons`.
        {
            let m = run(&[
                ("A1", "5"),
                ("B1", "=IF(A1>3,\"big\",\"small\")"),
                ("C1", "=IF(A1>10,1,0)"),
            ]);
            assert_eq!(val(&m, "B1"), CellValue::Text("big".into()));
            assert_eq!(val(&m, "C1"), CellValue::Number(0.0));
        }

        // Case `div_zero_and_cycle`.
        {
            let m = run(&[("A1", "=1/0"), ("B1", "=B2"), ("B2", "=B1")]);
            assert_eq!(val(&m, "A1"), CellValue::Error(FormulaError::Div0));
            assert!(val(&m, "B1").is_error());
        }

        // Case `specific_excel_error_codes`.
        {
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
    }

    /// Text functions, extended text functions and TEXT format codes.
    #[test]
    fn text_functions_and_format_codes() {
        // Case `text_functions`.
        {
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

        // Case `extended_text_functions`.
        {
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

        // Case `text_format_codes`.
        {
            assert_eq!(apply_text_code(1234.5, "#,##0.00"), "1,234.50");
            assert_eq!(apply_text_code(0.1234, "0.0%"), "12.3%");
            let serial = crate::model::date_to_serial(2023, 3, 5);
            assert_eq!(apply_text_code(serial, "yyyy-mm-dd"), "2023-03-05");
            assert_eq!(apply_text_code(serial, "d mmm yyyy"), "5 Mar 2023");
        }
    }

    /// COUNTIF/SUMIF, math, conditional statistics and logical functions.
    #[test]
    #[allow(
        clippy::approx_constant,
        reason = "the literals are spreadsheet inputs and expected outputs, not uses of pi or e"
    )]
    fn math_statistics_and_logical_functions() {
        // Case `countif_sumif`.
        {
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

        // Case `math_functions`.
        // ROUND(PI(),2) is expected to be 3.14; that is the assertion, not an
        // approximation of the constant.
        {
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

        // Case `stats_conditional_functions`.
        {
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

        // Case `logical_functions`.
        {
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
    }

    /// Lookup functions, date/time functions and TODAY.
    #[test]
    fn lookup_and_date_time_functions() {
        // Case `lookup_functions`.
        {
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

        // Case `date_time_functions`.
        {
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

        // Case `today_returns_a_plausible_recent_serial`.
        {
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
    }

    // --- format propagation -------------------------------------------------

    use crate::model::CellFormat;
    use tilo_locale::{Currency, DateStyle};

    fn fmts(pairs: &[(&str, CellFormat)]) -> HashMap<Addr, CellFormat> {
        pairs
            .iter()
            .map(|(a, f)| (a1_to_addr(a).unwrap(), *f))
            .collect()
    }

    /// Currency, percent, duration and unitless result formats.
    #[test]
    fn result_formats_propagate_through_arithmetic() {
        // Case `scalar_times_currency_is_currency`.
        {
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

        // Case `currency_plus_same_currency_propagates`.
        {
            let f = fmts(&[
                ("A1", CellFormat::Currency(Currency::EUR)),
                ("A2", CellFormat::Currency(Currency::EUR)),
            ]);
            assert_eq!(
                infer_result_format("A1+A2", &f),
                Some(CellFormat::Currency(Currency::EUR))
            );
        }

        // Case `mixed_currencies_fall_back_to_plain`.
        {
            let f = fmts(&[
                ("A1", CellFormat::Currency(Currency::USD)),
                ("A2", CellFormat::Currency(Currency::EUR)),
            ]);
            // USD + EUR is unclear → plain number (None).
            assert_eq!(infer_result_format("A1+A2", &f), None);
            // USD * EUR is also unclear.
            assert_eq!(infer_result_format("A1*A2", &f), None);
        }

        // Case `percent_arithmetic_propagates`.
        {
            let f = fmts(&[("A1", CellFormat::Percent), ("A2", CellFormat::Percent)]);
            assert_eq!(infer_result_format("A1+A2", &f), Some(CellFormat::Percent));
            assert_eq!(infer_result_format("A1*2", &f), Some(CellFormat::Percent));
        }

        // Case `sum_over_currency_range_inherits_currency`.
        {
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

        // Case `date_minus_date_is_a_duration_not_a_date`.
        {
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

        // Case `plain_arithmetic_has_no_format`.
        {
            let f = fmts(&[]);
            assert_eq!(infer_result_format("2+3*4", &f), None);
        }

        // Case `comparison_and_text_are_unitless`.
        {
            let f = fmts(&[("A1", CellFormat::Currency(Currency::USD))]);
            assert_eq!(infer_result_format("A1>100", &f), None);
            assert_eq!(infer_result_format("A1&\"x\"", &f), None);
        }

        // Case `growth_ratio_is_plain_even_with_currency_operands`.
        {
            // (E2-B2)/B2 where E2,B2 are currency → a ratio, which is unitless.
            let f = fmts(&[
                ("E2", CellFormat::Currency(Currency::USD)),
                ("B2", CellFormat::Currency(Currency::USD)),
            ]);
            // (currency - currency) = currency; currency / currency = Mixed→None.
            assert_eq!(infer_result_format("(E2-B2)/B2", &f), None);
        }
    }

    // --- reference extraction -----------------------------------------------

    /// Reference spans, range expansion/colour sharing and skipping strings/functions.
    #[test]
    fn references_report_spans_targets_and_colours() {
        // Case `references_single_cells_get_spans_and_targets`.
        {
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

        // Case `references_expand_ranges_and_share_colour_for_duplicates`.
        {
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

        // Case `references_skip_strings_and_functions`.
        {
            // `IF(B7>B9,"A1","B2")`: refs are B7 and B9 only; the quoted A1/B2 are
            // string literals and IF is a function name.
            let spans = references("IF(B7>B9,\"A1\",\"B2\")");
            let keys: Vec<&str> = spans.iter().map(|s| s.key.as_str()).collect();
            assert_eq!(keys, vec!["B7", "B9"]);
        }
    }

    // --- reference adjustment (copy/paste + fill) ---------------------------

    /// Relative/absolute offsets and structural remapping.
    #[test]
    fn reference_offsets_and_structural_remaps() {
        // Case `offset_refs_shifts_relative_preserves_absolute`.
        {
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

        // Case `offset_refs_leaves_functions_strings_and_numbers_alone`.
        {
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

        // Case `remap_refs_tracks_structural_moves_including_absolute_references`.
        {
            let moved = remap_refs("=SUM($A$1:B2)+\"A1\"+1e3", GridEdit::InsertRow(0));
            assert_eq!(moved, "=SUM($A$2:B3)+\"A1\"+1e3");

            let deleted = remap_refs("=SUM(A1:A3)+B2", GridEdit::DeleteRow(1));
            assert_eq!(deleted, "=SUM(A1:A2)+#REF!");
        }

        // Case `remap_refs_shrinks_a_range_whose_edge_line_is_deleted`.
        {
            // Rows (zero-based `DeleteRow(1)` is row 2): first, last, middle,
            // outside, and the whole range.
            let row = |raw| remap_refs(raw, GridEdit::DeleteRow(1));
            assert_eq!(row("=SUM(B2:B3)"), "=SUM(B2:B2)");
            assert_eq!(row("=SUM(B1:B2)"), "=SUM(B1:B1)");
            assert_eq!(row("=SUM(B1:B3)"), "=SUM(B1:B2)");
            assert_eq!(row("=SUM(B3:B4)"), "=SUM(B2:B3)");
            assert_eq!(row("=SUM(B1:C1)"), "=SUM(B1:C1)");
            assert_eq!(row("=SUM(B2:D2)"), "=SUM(#REF!)");
            // `$` anchors are spelled back as written; reversed corners keep
            // their roles.
            assert_eq!(row("=SUM($B$2:$B$3)"), "=SUM($B$2:$B$2)");
            assert_eq!(row("=SUM(B$2:$B3)"), "=SUM(B$2:$B2)");
            assert_eq!(row("=SUM(B3:B2)"), "=SUM(B2:B2)");
            assert_eq!(row("=SUM(B4 : B2)"), "=SUM(B3 : B2)");
            // A lone reference to the deleted row still breaks.
            assert_eq!(row("=B2+SUM(B2:B3)"), "=#REF!+SUM(B2:B2)");

            // Columns (`DeleteColumn(1)` is column B).
            let col = |raw| remap_refs(raw, GridEdit::DeleteColumn(1));
            assert_eq!(col("=SUM(B1:C1)"), "=SUM(B1:B1)");
            assert_eq!(col("=SUM(A1:B1)"), "=SUM(A1:A1)");
            assert_eq!(col("=SUM($A$1:$C$9)"), "=SUM($A$1:$B$9)");
            assert_eq!(col("=SUM(C1:D1)"), "=SUM(B1:C1)");
            assert_eq!(col("=SUM(B1:B9)"), "=SUM(#REF!)");
            assert_eq!(col("=SUM(C1:B1)"), "=SUM(B1:B1)");
        }

        // Case `remap_refs_grows_a_range_around_an_inserted_line`.
        {
            let row = |raw| remap_refs(raw, GridEdit::InsertRow(2));
            assert_eq!(row("=SUM(B2:B3)"), "=SUM(B2:B4)");
            assert_eq!(row("=SUM(B3:B4)"), "=SUM(B4:B5)");
            assert_eq!(row("=SUM(B1:B2)"), "=SUM(B1:B2)");
            let col = |raw| remap_refs(raw, GridEdit::InsertColumn(1));
            assert_eq!(col("=SUM(A1:B1)"), "=SUM(A1:C1)");
            assert_eq!(col("=SUM($A$1:$A$3)"), "=SUM($A$1:$A$3)");
        }
    }
}
