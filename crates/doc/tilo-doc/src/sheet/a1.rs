//! A1-style cell references ↔ `(column, row)` coordinates. Columns are 1-based
//! (`A` = 1, `Z` = 26, `AA` = 27, …); rows are 1-based (`A1` = row 1). Both the
//! `.xlsx` (`c r="A1"`) and `.ods` (implicit column position) codecs speak these.

/// Parse a column-letter run (`"A"`, `"Z"`, `"AA"`, …) into a 1-based index, or
/// `None` if it is empty or contains a non-letter. Bounded to 3 letters (`XFD`,
/// Excel's max column, is 3) so a pathological ref can't overflow.
pub fn col_to_index(letters: &str) -> Option<u32> {
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut idx: u32 = 0;
    for ch in letters.chars() {
        let c = ch.to_ascii_uppercase();
        if !c.is_ascii_uppercase() {
            return None;
        }
        // `c` is ASCII upper-case, so the offset from `A` always exists; the
        // multiply and add reject a run that would leave the u32 range.
        let offset = u32::from(c).checked_sub(u32::from('A'))?;
        idx = idx.checked_mul(26)?.checked_add(offset)?.checked_add(1)?;
    }
    Some(idx)
}

/// Renders a 1-based column index back into its letter run (`1` → `"A"`, `27` → `"AA"`); a zero index (invalid) renders as an empty string.
pub fn index_to_col(mut idx: u32) -> String {
    let mut out = Vec::new();
    // Step to a 0-based digit each round; a zero index has no digits at all, so
    // the loop condition is also what makes the subtraction safe.
    while let Some(zero_based) = idx.checked_sub(1) {
        // `% 26` keeps the remainder below 26, so it is entirely its low byte
        // and `A` + it stays inside `A`..=`Z`.
        let [digit, ..] = (zero_based % 26).to_le_bytes();
        out.push(b'A'.saturating_add(digit));
        idx = zero_based / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Splits an A1 reference (`"AB12"`) into `(column, row)` 1-based coordinates, or
/// `None` if malformed (missing letters/digits, or out of the bounded range); any `$` absolute-reference markers are ignored.
pub fn parse_ref(reference: &str) -> Option<(u32, u32)> {
    let cleaned: String = reference.chars().filter(|c| *c != '$').collect();
    let split = cleaned.find(|c: char| c.is_ascii_digit())?;
    if split == 0 {
        return None; // no column letters
    }
    let (letters, digits) = cleaned.split_at(split);
    let col = col_to_index(letters)?;
    // Bound the row so a malicious ref (`A99999999999999`) can't allocate a huge
    // grid on the reader side; Excel's max row is 1_048_576.
    let row: u32 = digits.parse().ok().filter(|r| *r >= 1 && *r <= 1_048_576)?;
    Some((col, row))
}

/// Build an A1 reference from 1-based `(column, row)` coordinates.
pub fn make_ref(col: u32, row: u32) -> String {
    format!("{}{}", index_to_col(col), row)
}

/// Excel's grid: `XFD` columns by 1,048,576 rows.
const MAX_COL: u32 = 16_384;
const MAX_ROW: u32 = 1_048_576;

/// Shifts every relative reference in an A1 formula by `d_cols` columns and
/// `d_rows` rows — how an `.xlsx` shared formula's template expands to the
/// other cells of its range (`B2*2` anchored at `C2` reads `B3*2` in `C3`).
/// `$`-absolute parts stay put; function names (a token followed by `(`),
/// `"text"` literals and `'quoted sheet'` names are left alone, and so is any
/// reference that would leave the grid.
pub fn shift_formula(formula: &str, d_cols: i64, d_rows: i64) -> String {
    let mut out = String::with_capacity(formula.len());
    let mut token = String::new();
    let mut quote: Option<char> = None;
    let mut chars = formula.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        if c.is_ascii_alphanumeric() || matches!(c, '$' | ':' | '_' | '.' | '\\') {
            token.push(c);
            if chars.peek().is_some() {
                continue;
            }
            // End of input: fall through with no separator.
            flush_token(&mut out, &mut token, false, d_cols, d_rows);
            continue;
        }
        flush_token(&mut out, &mut token, c == '(', d_cols, d_rows);
        out.push(c);
        if c == '"' || c == '\'' {
            quote = Some(c);
        }
    }
    out
}

fn flush_token(out: &mut String, token: &mut String, is_call: bool, d_cols: i64, d_rows: i64) {
    if token.is_empty() {
        return;
    }
    match (!is_call)
        .then(|| shift_range(token, d_cols, d_rows))
        .flatten()
    {
        Some(shifted) => out.push_str(&shifted),
        None => out.push_str(token),
    }
    token.clear();
}

/// One end of a reference: a cell (`$B$3`), a whole column (`B`) or a whole row (`3`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RefPart {
    Cell { col: (u32, bool), row: (u32, bool) },
    Col((u32, bool)),
    Row((u32, bool)),
}

fn shift_range(token: &str, d_cols: i64, d_rows: i64) -> Option<String> {
    let mut parts = token.split(':');
    let first = parse_part(parts.next()?)?;
    let Some(second) = parts.next() else {
        // A lone column or row name is not a reference.
        return matches!(first, RefPart::Cell { .. })
            .then(|| format_part(shift_part(first, d_cols, d_rows)?))
            .flatten();
    };
    if parts.next().is_some() {
        return None;
    }
    let second = parse_part(second)?;
    if std::mem::discriminant(&first) != std::mem::discriminant(&second) {
        return None;
    }
    Some(format!(
        "{}:{}",
        format_part(shift_part(first, d_cols, d_rows)?)?,
        format_part(shift_part(second, d_cols, d_rows)?)?
    ))
}

fn parse_part(part: &str) -> Option<RefPart> {
    let (abs_col, rest) = match part.strip_prefix('$') {
        Some(rest) => (true, rest),
        None => (false, part),
    };
    let letters_len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    let (letters, rest) = rest.split_at_checked(letters_len)?;
    let (abs_row, digits) = match rest.strip_prefix('$') {
        Some(digits) => (true, digits),
        None => (false, rest),
    };
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let col = (!letters.is_empty())
        .then(|| col_to_index(letters))
        .map(|c| c.map(|c| (c, abs_col)));
    let row = (!digits.is_empty())
        .then(|| digits.parse::<u32>().ok())
        .map(|r| r.map(|r| (r, if letters.is_empty() { abs_col } else { abs_row })));
    match (col, row) {
        (Some(col), Some(row)) => Some(RefPart::Cell {
            col: col?,
            row: row?,
        }),
        (Some(col), None) if !abs_row => Some(RefPart::Col(col?)),
        (None, Some(row)) if !abs_row => Some(RefPart::Row(row?)),
        _ => None,
    }
}

fn shift_axis((at, absolute): (u32, bool), by: i64, max: u32) -> Option<(u32, bool)> {
    if absolute {
        return Some((at, absolute));
    }
    let moved = u32::try_from(i64::from(at).checked_add(by)?).ok()?;
    (1..=max).contains(&moved).then_some((moved, absolute))
}

fn shift_part(part: RefPart, d_cols: i64, d_rows: i64) -> Option<RefPart> {
    Some(match part {
        RefPart::Cell { col, row } => RefPart::Cell {
            col: shift_axis(col, d_cols, MAX_COL)?,
            row: shift_axis(row, d_rows, MAX_ROW)?,
        },
        RefPart::Col(col) => RefPart::Col(shift_axis(col, d_cols, MAX_COL)?),
        RefPart::Row(row) => RefPart::Row(shift_axis(row, d_rows, MAX_ROW)?),
    })
}

fn format_part(part: RefPart) -> Option<String> {
    let dollar = |absolute: bool| if absolute { "$" } else { "" };
    let in_grid = |(at, _): (u32, bool), max: u32| (1..=max).contains(&at);
    Some(match part {
        RefPart::Cell { col, row } => {
            if !in_grid(col, MAX_COL) || !in_grid(row, MAX_ROW) {
                return None;
            }
            format!(
                "{}{}{}{}",
                dollar(col.1),
                index_to_col(col.0),
                dollar(row.1),
                row.0
            )
        }
        RefPart::Col(col) => {
            if !in_grid(col, MAX_COL) {
                return None;
            }
            format!("{}{}", dollar(col.1), index_to_col(col.0))
        }
        RefPart::Row(row) => {
            if !in_grid(row, MAX_ROW) {
                return None;
            }
            format!("{}{}", dollar(row.1), row.0)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a1_references_round_trip_and_reject_malformed_input() {
        for (letters, idx) in [
            ("A", 1u32),
            ("Z", 26),
            ("AA", 27),
            ("AZ", 52),
            ("BA", 53),
            ("ZZ", 702),
            ("AAA", 703),
        ] {
            assert_eq!(col_to_index(letters), Some(idx), "{letters} → {idx}");
            assert_eq!(index_to_col(idx), letters, "{idx} → {letters}");
        }
        for (r, col, row) in [
            ("A1", 1u32, 1u32),
            ("B2", 2, 2),
            ("AA10", 27, 10),
            ("XFD1048576", 16384, 1_048_576),
        ] {
            assert_eq!(parse_ref(r), Some((col, row)), "{r}");
            assert_eq!(make_ref(col, row), r, "({col},{row})");
        }
        assert_eq!(parse_ref("$B$3"), Some((2, 3)));
        for (r, why) in [
            ("", "empty"),
            ("1", "no column letters"),
            ("A", "no row digits"),
            ("A0", "row must be >= 1"),
            ("A2000000", "row over the bound"),
        ] {
            assert_eq!(parse_ref(r), None, "{why}");
        }
        assert_eq!(col_to_index("AAAA"), None, "over 3 letters");
        assert_eq!(col_to_index("A1"), None, "non-letter");
    }

    #[test]
    fn shared_formula_expansion_shifts_relative_parts_only() {
        assert_eq!(shift_formula("A1+$B1+C$1+$D$1", 3, 2), "D3+$B3+F$1+$D$1");
        assert_eq!(shift_formula("SUM(B2:B4)*2", 0, 1), "SUM(B3:B5)*2");
        assert_eq!(
            shift_formula("SUM(A:A)+SUM(2:3)", 1, 1),
            "SUM(B:B)+SUM(3:4)"
        );
        assert_eq!(
            shift_formula("SUM(Sheet1!A1,'My A1'!$B2)", 1, 1),
            "SUM(Sheet1!B2,'My A1'!$B3)"
        );
        assert_eq!(
            shift_formula("CONCATENATE(A1,\"A1\",LOG10(B1),TRUE,0.5)", 0, 1),
            "CONCATENATE(A2,\"A1\",LOG10(B2),TRUE,0.5)"
        );
        assert_eq!(shift_formula("A1+XFD1", 1, 0), "B1+XFD1");
        assert_eq!(shift_formula("B2", -1, -1), "A1");
        assert_eq!(shift_formula("A1", -1, 0), "A1");
        assert_eq!(shift_formula("", 1, 1), "");
    }
}
