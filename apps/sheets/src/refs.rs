//! Cell references inside formula text: where they are, for tinting them
//! while a formula is edited, and how they move when a formula is copied,
//! rows or columns are inserted or deleted, or a sheet is renamed.
//!
//! These work on the raw formula a cell stores, character by character, so
//! the formula keeps its spelling (spaces, case, `$` anchors) through every
//! rewrite; the formula engine only ever reads the result.

use std::collections::HashMap;

use crate::model::{Addr, GridEdit, a1_to_addr, addr_to_a1};

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
}

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
/// are never mistaken for references. A reference to another sheet
/// (`Rates!A2`) names no cell of this one and is not returned.
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
    rewrite_refs(raw, true, |first, second| match second {
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
    rewrite_refs(raw, false, |first, second| {
        let moved = match second {
            Some((sep, second)) => edit
                .map_range(first.addr, second.addr)
                .map(|(a, b)| format!("{}{sep}{}", first.render(a), second.render(b))),
            None => edit.map_addr(first.addr).map(|addr| first.render(addr)),
        };
        moved.unwrap_or_else(|| "#REF!".into())
    })
}

/// `raw` with the sheet each reference to another sheet names replaced by
/// what `rename` answers for it: `rename` gets the name as written, quotes
/// undone, and answers the name to write instead, or `None` to keep it as
/// written. String literals are left alone, and input that is no formula is
/// returned unchanged.
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

/// A sheet name as references match it, ignoring case and surrounding
/// space, as spreadsheets do.
pub fn fold_sheet(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Rewrite every reference in a formula through `rewrite`, which receives a
/// lone reference or both ends of a range with the separator between them.
/// String literals, sheet names, function names, and numeric exponents
/// (`1e3`) are copied verbatim; non-formula input (no leading `=`) is
/// returned unchanged. A reference to another sheet is rewritten only when
/// `foreign` is set: a copy shifts it as any other, but rows and columns
/// inserted on this sheet do not move it.
fn rewrite_refs(
    raw: &str,
    foreign: bool,
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

/// At a sheet name and its `!` (`Rates!`, `'Q3 data'!`), move `scan` past
/// both and answer the text they span; otherwise leave it and answer `None`.
fn sheet_prefix(scan: &mut Scan<'_>) -> Option<String> {
    let start = scan.pos();
    let mut ahead = *scan;
    if ahead.peek() == Some('\'') {
        quoted_sheet(&mut ahead)?;
    } else {
        // A name right after a digit is a number's exponent, not a sheet.
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

/// Move `scan` past the reference at it, and the range end after a `:`.
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

/// Whether `c` may be part of an unquoted name: a function's, a
/// reference's or a sheet's.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '$' || c == '_' || c == '.'
}

/// A quoted sheet name and the `!` after it (`'Q3 data'!`), the scan at its
/// opening quote: the name, a doubled quote standing for one. `None` when
/// the quote is not closed or no `!` follows.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference spans, range expansion/colour sharing and skipping strings/functions.
    #[test]
    fn references_report_spans_targets_and_colours() {
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

        {
            // `SUM(B2:D2)+B2`: the range B2:D2 and the single B2 are different
            // keys; a repeated B2 would reuse a slot.
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

            assert_eq!(offset_refs("=SUM(A1:B3)", 1, 1), "=SUM(B2:C4)");
            // A range mixing anchored and relative endpoints shifts only the free one.
            assert_eq!(offset_refs("=$A$1:B2", 1, 1), "=$A$1:C3");
        }

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

        {
            let moved = remap_refs("=SUM($A$1:B2)+\"A1\"+1e3", GridEdit::InsertRow(0));
            assert_eq!(moved, "=SUM($A$2:B3)+\"A1\"+1e3");

            let deleted = remap_refs("=SUM(A1:A3)+B2", GridEdit::DeleteRow(1));
            assert_eq!(deleted, "=SUM(A1:A2)+#REF!");
        }

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
