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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_roundtrip() {
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
    }

    #[test]
    fn ref_roundtrip() {
        for (r, col, row) in [
            ("A1", 1u32, 1u32),
            ("B2", 2, 2),
            ("AA10", 27, 10),
            ("XFD1048576", 16384, 1_048_576),
        ] {
            assert_eq!(parse_ref(r), Some((col, row)), "{r}");
            assert_eq!(make_ref(col, row), r, "({col},{row})");
        }
    }

    #[test]
    fn absolute_markers_ignored() {
        assert_eq!(parse_ref("$B$3"), Some((2, 3)));
    }

    #[test]
    fn rejects_malformed_and_out_of_range() {
        assert_eq!(parse_ref(""), None);
        assert_eq!(parse_ref("1"), None, "no column letters");
        assert_eq!(parse_ref("A"), None, "no row digits");
        assert_eq!(parse_ref("A0"), None, "row must be >= 1");
        assert_eq!(parse_ref("A2000000"), None, "row over the bound");
        assert_eq!(col_to_index("AAAA"), None, "over 3 letters");
        assert_eq!(col_to_index("A1"), None, "non-letter");
    }
}
