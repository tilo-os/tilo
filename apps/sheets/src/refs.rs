//! Cell references inside formula text: where they are, for tinting, and how they move when a
//! formula is copied or rows, columns or sheet names change. Everything works on the raw text,
//! so a formula keeps its spelling through every rewrite.

use std::collections::HashMap;

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

/// A colour slot per span, in first-appearance order, so identical references share one.
pub fn ref_color_slots(spans: &[RefSpan]) -> Vec<usize> {
    let mut map: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::with_capacity(spans.len());
    for s in spans {
        let next = map.len();
        out.push(*map.entry(s.key.as_str()).or_insert(next));
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
    let shift = |token: RefToken| token.render(token.offset(dcol, drow));
    rewrite_refs(raw, true, |reference| match reference {
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
}

/// A formula rewritten through a structural grid change: unlike a copy, inserting or deleting a
/// row or column moves anchored references too, a deleted target becomes `#REF!`, and a range
/// moves as a unit, shrinking when its first or last line goes.
pub fn remap_refs(raw: &str, edit: GridEdit) -> String {
    rewrite_refs(raw, false, |reference| {
        let moved = match reference {
            Ref::Cell(first) => edit.map_addr(first.addr).map(|addr| first.render(addr)),
            Ref::Range(first, sep, second) => edit
                .map_range(first.addr, second.addr)
                .map(|(a, b)| format!("{}{sep}{}", first.render(a), second.render(b))),
            Ref::Lines(first, sep, second) => {
                let rows_edit = matches!(edit, GridEdit::InsertRow(_) | GridEdit::DeleteRow(_));
                if first.rows == rows_edit {
                    // Whole rows move only with rows, whole columns only with columns.
                    let corner = |line: Line| {
                        if line.rows {
                            (0, line.at)
                        } else {
                            (line.at, 0)
                        }
                    };
                    let along = |(col, row): Addr| if first.rows { row } else { col };
                    edit.map_range(corner(first), corner(second)).map(|(a, b)| {
                        format!("{}{sep}{}", first.render(along(a)), second.render(along(b)))
                    })
                } else {
                    Some(format!(
                        "{}{sep}{}",
                        first.render(first.at),
                        second.render(second.at)
                    ))
                }
            }
        };
        moved.unwrap_or_else(|| "#REF!".into())
    })
}

/// A formula with each reference to another sheet renamed by `rename`, which gets the name as
/// written and answers the one to write instead.
pub fn rename_sheets(raw: &str, mut rename: impl FnMut(&str) -> Option<String>) -> String {
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

/// One reference the rewrite hands its callback: a cell, a range, or whole rows or columns.
enum Ref<'a> {
    Cell(RefToken),
    Range(RefToken, &'a str, RefToken),
    Lines(Line, &'a str, Line),
}

/// Every reference in a formula rewritten through `rewrite`; literals, sheet names, function
/// names and exponents are copied verbatim. A foreign reference is rewritten only when asked,
/// since rows inserted on this sheet do not move it.
fn rewrite_refs(raw: &str, foreign: bool, mut rewrite: impl FnMut(Ref<'_>) -> String) -> String {
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
        if let Some(sheet) = sheet_prefix(&mut scan) {
            out.push_str(&sheet);
            if !foreign {
                let start = scan.pos();
                skip_reference(&mut scan);
                out.push_str(&scan.text_from(start));
            }
            continue;
        }
        if let Some((first, sep, second, after)) = line_range(scan) {
            scan = after;
            out.push_str(&rewrite(Ref::Lines(first, &sep, second)));
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
        // A name followed by `(` is a function.
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
                out.push_str(&rewrite(Ref::Range(first, &sep, second)));
                continue;
            }
        }
        out.push_str(&rewrite(Ref::Cell(first)));
    }
    out
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

/// A quoted sheet name and the `!` after it, a doubled quote standing for one.
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
struct Line {
    /// The row or column, zero-based.
    at: u32,
    rows: bool,
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
    fn render(self, at: u32) -> String {
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
struct RefToken {
    addr: Addr,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_report_spans_targets_and_colours() {
        {
            let spans = references("B9-B7");
            assert_eq!(spans.len(), 2);
            assert_eq!(spans[0].tl, a1_to_addr("B9").unwrap());
            assert_eq!(spans[0].br, a1_to_addr("B9").unwrap());
            assert_eq!(spans[1].tl, a1_to_addr("B7").unwrap());
            assert_eq!(&"B9-B7"[spans[0].start..spans[0].end], "B9");
            assert_eq!(&"B9-B7"[spans[1].start..spans[1].end], "B7");
            let slots = ref_color_slots(&spans);
            assert_eq!(slots, vec![0, 1], "two different refs → two colours");
        }

        {
            let spans = references("SUM(B2:D2)+B2");
            assert_eq!(spans.len(), 2, "SUM is a call, not a ref");
            assert_eq!(spans[0].key, "B2:D2");
            assert_eq!(spans[0].tl, a1_to_addr("B2").unwrap());
            assert_eq!(spans[0].br, a1_to_addr("D2").unwrap());
            assert_eq!(spans[1].key, "B2");

            let dup = references("A1+A1");
            let slots = ref_color_slots(&dup);
            assert_eq!(slots, vec![0, 0]);
        }

        {
            let spans = references("IF(B7>B9,\"A1\",\"B2\")");
            let keys: Vec<&str> = spans.iter().map(|s| s.key.as_str()).collect();
            assert_eq!(keys, vec!["B7", "B9"]);
        }
    }

    #[test]
    fn reference_offsets_and_structural_remaps() {
        {
            assert_eq!(offset_refs("=A1", 0, 1), "=A2");
            assert_eq!(offset_refs("=A1", 1, 0), "=B1");
            assert_eq!(offset_refs("=A1", 1, 1), "=B2");

            // `$A$1` is fully anchored and never moves anywhere.
            assert_eq!(offset_refs("=$A$1", 3, 4), "=$A$1");
            assert_eq!(offset_refs("=$A1", 5, 2), "=$A3");
            assert_eq!(offset_refs("=A$1", 2, 5), "=C$1");

            assert_eq!(offset_refs("=SUM(A1:B3)", 1, 1), "=SUM(B2:C4)");
            assert_eq!(offset_refs("=$A$1:B2", 1, 1), "=$A$1:C3");
        }

        {
            // Function names and quoted string literals are never references.
            assert_eq!(
                offset_refs("=IF(A1>0,\"A1\",B2)", 0, 1),
                "=IF(A2>0,\"A1\",B3)"
            );
            assert_eq!(offset_refs("=1e3+A1", 0, 1), "=1e3+A2");
            assert_eq!(offset_refs("1200", 3, 3), "1200");
            assert_eq!(offset_refs("=A1+B2", 0, 0), "=A1+B2");
            // A relative component never underflows past the grid origin.
            assert_eq!(offset_refs("=A1", -5, -5), "=A1");
        }

        {
            let moved = remap_refs("=SUM($A$1:B2)+\"A1\"+1e3", GridEdit::InsertRow(0));
            assert_eq!(moved, "=SUM($A$2:B3)+\"A1\"+1e3");

            let deleted = remap_refs("=SUM(A1:A3)+B2", GridEdit::DeleteRow(1));
            assert_eq!(deleted, "=SUM(A1:A2)+#REF!");
        }

        {
            let row = |raw| remap_refs(raw, GridEdit::DeleteRow(1));
            assert_eq!(row("=SUM(B2:B3)"), "=SUM(B2:B2)");
            assert_eq!(row("=SUM(B1:B2)"), "=SUM(B1:B1)");
            assert_eq!(row("=SUM(B1:B3)"), "=SUM(B1:B2)");
            assert_eq!(row("=SUM(B3:B4)"), "=SUM(B2:B3)");
            assert_eq!(row("=SUM(B1:C1)"), "=SUM(B1:C1)");
            assert_eq!(row("=SUM(B2:D2)"), "=SUM(#REF!)");
            assert_eq!(row("=SUM($B$2:$B$3)"), "=SUM($B$2:$B$2)");
            assert_eq!(row("=SUM(B$2:$B3)"), "=SUM(B$2:$B2)");
            assert_eq!(row("=SUM(B3:B2)"), "=SUM(B2:B2)");
            assert_eq!(row("=SUM(B4 : B2)"), "=SUM(B3 : B2)");
            assert_eq!(row("=B2+SUM(B2:B3)"), "=#REF!+SUM(B2:B2)");

            let col = |raw| remap_refs(raw, GridEdit::DeleteColumn(1));
            assert_eq!(col("=SUM(B1:C1)"), "=SUM(B1:B1)");
            assert_eq!(col("=SUM(A1:B1)"), "=SUM(A1:A1)");
            assert_eq!(col("=SUM($A$1:$C$9)"), "=SUM($A$1:$B$9)");
            assert_eq!(col("=SUM(C1:D1)"), "=SUM(B1:C1)");
            assert_eq!(col("=SUM(B1:B9)"), "=SUM(#REF!)");
            assert_eq!(col("=SUM(C1:B1)"), "=SUM(B1:B1)");
        }

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

    #[test]
    fn whole_rows_and_columns_move_like_ranges() {
        assert_eq!(offset_refs("=SUM(1:1)", 3, 1), "=SUM(2:2)");
        assert_eq!(offset_refs("=SUM(A:A)", 1, 3), "=SUM(B:B)");
        assert_eq!(offset_refs("=SUM($1:3)", 0, 2), "=SUM($1:5)");
        assert_eq!(offset_refs("=SUM(A:$C)", 2, 0), "=SUM(C:$C)");
        assert_eq!(offset_refs("=SUM(2 : 4)", 0, -5), "=SUM(1 : 1)");
        assert_eq!(offset_refs("=SUM(Rates!B:C)", 1, 0), "=SUM(Rates!C:D)");
        assert_eq!(offset_refs("=1+2*A1", 0, 1), "=1+2*A2");
        assert_eq!(offset_refs("=A1:B2", 0, 1), "=A2:B3");
        assert_eq!(offset_refs("=\"1:1\"&A1", 0, 1), "=\"1:1\"&A2");

        assert_eq!(remap_refs("=SUM(2:3)", GridEdit::InsertRow(0)), "=SUM(3:4)");
        assert_eq!(
            remap_refs("=SUM($2:$3)", GridEdit::InsertRow(2)),
            "=SUM($2:$4)"
        );
        assert_eq!(remap_refs("=SUM(2:3)", GridEdit::DeleteRow(1)), "=SUM(2:2)");
        assert_eq!(
            remap_refs("=SUM(2:2)", GridEdit::DeleteRow(1)),
            "=SUM(#REF!)"
        );
        assert_eq!(
            remap_refs("=SUM(2:3)", GridEdit::InsertColumn(0)),
            "=SUM(2:3)"
        );
        assert_eq!(
            remap_refs("=SUM(B:C)", GridEdit::InsertColumn(0)),
            "=SUM(C:D)"
        );
        assert_eq!(
            remap_refs("=SUM(B:C)", GridEdit::DeleteColumn(2)),
            "=SUM(B:B)"
        );
        assert_eq!(
            remap_refs("=SUM(A:A)", GridEdit::DeleteColumn(0)),
            "=SUM(#REF!)"
        );
        assert_eq!(remap_refs("=SUM(B:C)", GridEdit::DeleteRow(0)), "=SUM(B:C)");
        assert_eq!(
            remap_refs("=SUM(Rates!2:3)", GridEdit::InsertRow(0)),
            "=SUM(Rates!2:3)"
        );
    }

    #[test]
    fn references_to_other_sheets() {
        assert_eq!(
            remap_refs("=A2+Rates!A2+'Q 3'!B2:B4", GridEdit::InsertRow(0)),
            "=A3+Rates!A2+'Q 3'!B2:B4",
            "rows inserted on this sheet do not move them"
        );
        assert_eq!(
            offset_refs("=Rates!A2", 0, 1),
            "=Rates!A3",
            "a copy shifts them as any other"
        );
        assert_eq!(references("Rates!A2+B1").len(), 1);
        let renamed = |raw: &str| {
            rename_sheets(raw, |written| {
                (fold_sheet(written) == "rates").then(|| "Q3 rates".to_owned())
            })
        };
        assert_eq!(
            renamed("=rates!A1+'Rates'!B2:B3+\"Rates!A1\"+Other!A1+A1"),
            "='Q3 rates'!A1+'Q3 rates'!B2:B3+\"Rates!A1\"+Other!A1+A1"
        );
        assert_eq!(renamed("Rates!A1"), "Rates!A1", "no formula");
    }
}
