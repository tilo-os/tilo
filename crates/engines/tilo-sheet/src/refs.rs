//! Cell references inside formula text: where they are, for tinting, and how they move when a
//! formula is copied or rows, columns or sheets change, on the formula's own sheet or on one it
//! names. Everything works on the raw text, so a formula keeps its spelling through every
//! rewrite.

use crate::model::{Addr, GridEdit, a1_to_addr, addr_to_a1};

/// A bounds-checked cursor over a formula's characters: every read goes through it, so a
/// malformed formula ends the scan instead of reading past the end. `Copy`, so a caller can
/// fork a look-ahead and commit it once that succeeded.
#[derive(Clone, Copy)]
struct Scan<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> Scan<'a> {
    fn new(chars: &'a [char]) -> Self {
        Self { chars, pos: 0 }
    }

    fn pos(self) -> usize {
        self.pos
    }

    fn peek(self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn before(self, index: usize) -> Option<char> {
        index
            .checked_sub(1)
            .and_then(|prev| self.chars.get(prev))
            .copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos = self.pos.saturating_add(1);
        Some(c)
    }

    fn skip_while(&mut self, pred: impl Fn(char) -> bool) {
        while self.peek().is_some_and(&pred) {
            self.pos = self.pos.saturating_add(1);
        }
    }

    fn text_from(self, start: usize) -> String {
        self.chars
            .get(start..self.pos)
            .unwrap_or_default()
            .iter()
            .collect()
    }
}

/// A cell or range reference inside a formula body, tinted in the text and outlined on the grid.
#[derive(Clone, Debug, PartialEq)]
pub struct RefSpan {
    pub start: usize,
    pub end: usize,
    /// The rectangle's top-left cell, equal to `br` for one cell.
    pub tl: Addr,
    pub br: Addr,
    /// Identical references share a key, and with it a colour.
    pub key: String,
}

/// Every cell and range reference in a formula body, with its span and rectangle. String
/// literals are skipped, a name followed by `(` is a function, `1e3` is an exponent, and a
/// reference to another sheet is not returned.
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
        if sheet_prefix(&mut scan).is_some() {
            skip_reference(&mut scan);
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
        // A name followed by `(` is a function.
        let mut ahead = scan;
        ahead.skip_while(char::is_whitespace);
        if prev_numish || ahead.peek() == Some('(') {
            continue;
        }
        let Some(first) = a1_to_addr(&scan.text_from(start)) else {
            continue; // TRUE / FALSE / function names / bare words
        };
        // A `:`, optionally spaced, to another reference makes a range from one to the other.
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

/// A formula whose relative references shift by `(dcol, drow)`, as a copy or fill moves them:
/// `$A1` locks the column, `A$1` the row, `$A$1` both; a range shifts each end; literals,
/// function names and exponents stay as written. A component never underflows past column `A`
/// or row 1.
pub fn offset_refs(raw: &str, dcol: i64, drow: i64) -> String {
    if dcol == 0 && drow == 0 {
        return raw.to_string();
    }
    let shift = |token: &RefToken| token.render(token.offset(dcol, drow));
    rewrite_refs(raw, &mut |_, reference| {
        Moved::To(match reference {
            Ref::Cell(first) => shift(first),
            Ref::Range(first, sep, second) => format!("{}{sep}{}", shift(first), shift(second)),
            Ref::Lines(first, sep, second) => {
                let by = if first.rows { drow } else { dcol };
                format!(
                    "{}{sep}{}",
                    first.render(first.offset(by)),
                    second.render(second.offset(by))
                )
            }
        })
    })
}

/// A formula rewritten through a structural change of its own sheet: unlike a copy, inserting or
/// deleting a row or column moves anchored references too, a deleted target becomes `#REF!`, and
/// a range moves as a unit, shrinking when its first or last line goes. A reference that names a
/// sheet stays as written: [`remap_sheet_refs`] moves those.
pub fn remap_refs(raw: &str, edit: GridEdit) -> String {
    remap_sheet_refs(raw, edit, true, None)
}

/// [`remap_refs`] for any formula of the workbook, through a structural change of one sheet.
/// `own` says the formula is on that sheet, so its bare references move; `named` is the name
/// formulas reach that sheet by, folded as [`fold_sheet`] does, and a reference naming it moves
/// wherever it is written. A deleted target becomes `#REF!` in place of the name too: the
/// engine reads no error after a sheet's name. A reference the change does not move keeps its
/// spelling.
pub fn remap_sheet_refs(raw: &str, edit: GridEdit, own: bool, named: Option<&str>) -> String {
    rewrite_refs(raw, &mut |sheet, reference| {
        let on_changed_sheet = match sheet {
            None => own,
            Some(written) => named.is_some_and(|named| fold_sheet(written) == named),
        };
        if !on_changed_sheet {
            return Moved::Kept;
        }
        let moved = match reference {
            Ref::Cell(first) => match edit.map_addr(first.addr) {
                Some(addr) if addr == first.addr => return Moved::Kept,
                Some(addr) => Some(first.render(addr)),
                None => None,
            },
            Ref::Range(first, sep, second) => match edit.map_range(first.addr, second.addr) {
                Some((a, b)) if a == first.addr && b == second.addr => return Moved::Kept,
                Some((a, b)) => Some(format!("{}{sep}{}", first.render(a), second.render(b))),
                None => None,
            },
            Ref::Lines(first, sep, second) => {
                let rows_edit = matches!(edit, GridEdit::InsertRow(_) | GridEdit::DeleteRow(_));
                if first.rows != rows_edit {
                    // Whole rows move only with rows, whole columns only with columns.
                    return Moved::Kept;
                }
                let corner = |line: &Line| {
                    if line.rows {
                        (0, line.at)
                    } else {
                        (line.at, 0)
                    }
                };
                let along = |(col, row): Addr| if first.rows { row } else { col };
                match edit.map_range(corner(first), corner(second)) {
                    Some((a, b)) if along(a) == first.at && along(b) == second.at => {
                        return Moved::Kept;
                    }
                    Some((a, b)) => Some(format!(
                        "{}{sep}{}",
                        first.render(along(a)),
                        second.render(along(b))
                    )),
                    None => None,
                }
            }
        };
        moved.map_or(Moved::Gone, Moved::To)
    })
}

/// A formula with each reference to another sheet renamed by `rename`, which gets the name as
/// written and answers the one to write instead.
pub fn rename_sheets(raw: &str, rename: &mut dyn FnMut(&str) -> Option<String>) -> String {
    let Some(body) = raw.strip_prefix('=').filter(|body| body.contains('!')) else {
        return raw.to_string();
    };
    let chars: Vec<char> = body.chars().collect();
    let mut scan = Scan::new(&chars);
    let mut out = String::with_capacity(raw.len());
    out.push('=');
    while let Some(ch) = scan.peek() {
        if ch == '"' {
            let start = scan.pos();
            scan.bump();
            scan.skip_while(|c| c != '"');
            scan.bump();
            out.push_str(&scan.text_from(start));
            continue;
        }
        if let Some(prefix) = sheet_prefix(&mut scan) {
            match rename(&written_sheet(&prefix)) {
                Some(name) => {
                    out.push_str(&ironcalc_base::expressions::utils::quote_name(&name));
                    out.push('!');
                }
                None => out.push_str(&prefix),
            }
            continue;
        }
        out.push(ch);
        scan.bump();
    }
    out
}

/// The sheet name a `Rates!` or `'Q3 data'!` prefix spells.
fn written_sheet(prefix: &str) -> String {
    let name = prefix.strip_suffix('!').unwrap_or(prefix);
    match name
        .strip_prefix('\'')
        .and_then(|name| name.strip_suffix('\''))
    {
        Some(quoted) => quoted.replace("''", "'"),
        None => name.to_string(),
    }
}

/// A sheet name as references match it: case and surrounding space ignored.
pub fn fold_sheet(name: &str) -> String {
    name.trim().to_lowercase()
}

/// One reference the rewrite hands its callback: a cell, a range, or whole rows or columns, the
/// last two with what is written between their ends.
pub(crate) enum Ref {
    Cell(RefToken),
    Range(RefToken, String, RefToken),
    Lines(Line, String, Line),
}

impl Ref {}

/// What a rewrite does with one reference.
pub(crate) enum Moved {
    /// As written, with the sheet it names.
    Kept,
    /// Written anew, after the sheet it names.
    To(String),
    /// `#REF!` in its place and in the place of the sheet it names.
    Gone,
}

/// Every reference in a formula rewritten through `rewrite`, which gets the sheet the reference
/// names as written (`None` for the formula's own sheet); literals, function names and
/// exponents are copied verbatim.
pub(crate) fn rewrite_refs(
    raw: &str,
    rewrite: &mut dyn FnMut(Option<&str>, &Ref) -> Moved,
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
            let start = scan.pos();
            scan.bump();
            scan.skip_while(|c| c != '"');
            scan.bump();
            out.push_str(&scan.text_from(start));
            continue;
        }
        let start = scan.pos();
        let prefix = sheet_prefix(&mut scan);
        let (reference, after) = reference_at(scan);
        let Some(reference) = reference else {
            if after.pos() == start {
                out.push(ch);
                scan.bump();
            } else {
                // A sheet's name or a word that is no reference, as written.
                scan = after;
                out.push_str(&scan.text_from(start));
            }
            continue;
        };
        scan = after;
        let sheet = prefix.as_deref().map(written_sheet);
        match rewrite(sheet.as_deref(), &reference) {
            Moved::Kept => out.push_str(&scan.text_from(start)),
            Moved::To(text) => {
                out.push_str(prefix.as_deref().unwrap_or_default());
                out.push_str(&text);
            }
            Moved::Gone => out.push_str("#REF!"),
        }
    }
    out
}

/// The reference at the scan, and the scan past it: whole rows or columns, a range or a cell.
/// A function's name, a bare word and a number's exponent are no reference, and the scan is past
/// that word.
fn reference_at(scan: Scan<'_>) -> (Option<Ref>, Scan<'_>) {
    if let Some((first, sep, second, after)) = line_range(scan) {
        return (Some(Ref::Lines(first, sep, second)), after);
    }
    if !scan
        .peek()
        .is_some_and(|c| c == '$' || c.is_ascii_alphabetic())
    {
        return (None, scan);
    }
    let start = scan.pos();
    let mut scan = scan;
    scan.skip_while(|c| c == '$' || c.is_ascii_alphanumeric());
    // A run right after a digit/dot is an exponent (`1e3`), not a ref.
    let prev_numish = scan
        .before(start)
        .is_some_and(|prev| prev.is_ascii_digit() || prev == '.');
    // A name followed by `(` is a function.
    let mut ahead = scan;
    ahead.skip_while(char::is_whitespace);
    let is_call = ahead.peek() == Some('(');
    let Some(first) = RefToken::parse(&scan.text_from(start)).filter(|_| !prev_numish && !is_call)
    else {
        return (None, scan);
    };
    if ahead.peek() == Some(':') {
        ahead.bump();
        ahead.skip_while(char::is_whitespace);
        let sep = ahead.text_from(scan.pos());
        let end_start = ahead.pos();
        ahead.skip_while(|c| c == '$' || c.is_ascii_alphanumeric());
        if let Some(second) = RefToken::parse(&ahead.text_from(end_start)) {
            return (Some(Ref::Range(first, sep, second)), ahead);
        }
    }
    (Some(Ref::Cell(first)), scan)
}

/// A sheet name and its `!`, with the scan moved past both.
fn sheet_prefix(scan: &mut Scan<'_>) -> Option<String> {
    let start = scan.pos();
    let mut ahead = *scan;
    if ahead.peek() == Some('\'') {
        quoted_sheet(&mut ahead)?;
    } else {
        // A name after a digit is an exponent, not a sheet.
        if ahead
            .before(start)
            .is_some_and(|prev| prev.is_ascii_alphanumeric() || prev == '.')
        {
            return None;
        }
        ahead.skip_while(is_name_char);
        if ahead.pos() == start || ahead.bump() != Some('!') {
            return None;
        }
    }
    *scan = ahead;
    Some(scan.text_from(start))
}

fn skip_reference(scan: &mut Scan<'_>) {
    scan.skip_while(|c| c == '$' || c.is_ascii_alphanumeric());
    let mut ahead = *scan;
    ahead.skip_while(char::is_whitespace);
    if ahead.peek() == Some(':') {
        ahead.bump();
        ahead.skip_while(char::is_whitespace);
        ahead.skip_while(|c| c == '$' || c.is_ascii_alphanumeric());
        *scan = ahead;
    }
}

/// Whether `c` may be part of an unquoted name.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '$' || c == '_' || c == '.'
}

/// A quoted sheet name and the `!` after it, a doubled quote for one.
fn quoted_sheet(scan: &mut Scan<'_>) -> Option<String> {
    scan.bump();
    let mut name = String::new();
    loop {
        match scan.bump()? {
            '\'' if scan.peek() == Some('\'') => {
                scan.bump();
                name.push('\'');
            }
            '\'' => break,
            c => name.push(c),
        }
    }
    (scan.bump() == Some('!')).then_some(name)
}

/// One end of a whole-row or whole-column range, remembering its `$`.
#[derive(Clone, Copy)]
pub(crate) struct Line {
    /// The row or column, zero-based.
    pub(crate) at: u32,
    pub(crate) rows: bool,
    abs: bool,
}

impl Line {
    /// The line at the scan, a row number or column letters.
    fn parse(scan: &mut Scan<'_>) -> Option<Self> {
        let abs = scan.peek() == Some('$');
        if abs {
            scan.bump();
        }
        let start = scan.pos();
        let rows = scan.peek()?.is_ascii_digit();
        if rows {
            scan.skip_while(|c| c.is_ascii_digit());
        } else {
            scan.skip_while(|c| c.is_ascii_alphabetic());
        }
        let text = scan.text_from(start);
        let at = if rows {
            text.parse::<u32>().ok()?.checked_sub(1)?
        } else if (1..=3).contains(&text.len()) {
            crate::model::letters_to_col(&text)?
        } else {
            return None;
        };
        Some(Self { at, rows, abs })
    }

    /// The line moved by a copy or fill; an anchored one stays.
    fn offset(self, by: i64) -> u32 {
        if self.abs {
            return self.at;
        }
        let moved = i64::from(self.at)
            .saturating_add(by)
            .clamp(0, i64::from(u32::MAX));
        u32::try_from(moved).unwrap_or(self.at)
    }

    /// Line `at` spelled with this end's `$`.
    pub(crate) fn render(self, at: u32) -> String {
        let anchor = if self.abs { "$" } else { "" };
        if self.rows {
            format!("{anchor}{}", at.saturating_add(1))
        } else {
            format!("{anchor}{}", crate::model::col_to_letters(at))
        }
    }
}

/// A whole-row or whole-column range at the scan, with the scan past it.
fn line_range(scan: Scan<'_>) -> Option<(Line, String, Line, Scan<'_>)> {
    if scan.before(scan.pos()).is_some_and(is_name_char) {
        return None;
    }
    let mut ahead = scan;
    let first = Line::parse(&mut ahead)?;
    let sep_start = ahead.pos();
    ahead.skip_while(char::is_whitespace);
    if ahead.bump() != Some(':') {
        return None;
    }
    ahead.skip_while(char::is_whitespace);
    let sep = ahead.text_from(sep_start);
    let second = Line::parse(&mut ahead)?;
    if second.rows != first.rows || ahead.peek().is_some_and(|c| is_name_char(c) || c == '(') {
        return None;
    }
    Some((first, sep, second, ahead))
}

/// An `A1` token remembering its `$` spelling, as written.
#[derive(Clone, Copy)]
pub(crate) struct RefToken {
    pub(crate) addr: Addr,
    col_abs: bool,
    row_abs: bool,
}

impl RefToken {
    /// The token, or `None` when it is not a well-formed reference.
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

    /// The address moved by a copy or fill: anchored components stay and the rest clamp to the
    /// grid's addressable range.
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

    /// The cell `(col, row)` spelled with this token's `$`.
    pub(crate) fn render(self, (col, row): Addr) -> String {
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
