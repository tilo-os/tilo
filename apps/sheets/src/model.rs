//! Pure data model for the spreadsheet: cell addressing (A1 notation),
//! cell values, and per-cell number/text/date formats.
//!
//! Nothing here touches the CRDT or the renderer — it's the shared
//! vocabulary the formula engine, the grid widget, and the document
//! layer all speak. Cells are addressed internally by zero-based
//! `(col, row)`; `A1` is `(0, 0)`. The `col_to_letters` / `a1_to_addr`
//! helpers translate to and from the user-facing A1 strings.

/// Zero-based column index. `0` is column `A`.
pub type Col = u32;
/// Zero-based row index. `0` is row `1`.
pub type Row = u32;

/// A cell coordinate, `(col, row)`, zero-based.
pub type Addr = (Col, Row);

/// Convert a zero-based column index to spreadsheet letters.
/// `0 -> "A"`, `25 -> "Z"`, `26 -> "AA"`, `701 -> "ZZ"`.
pub fn col_to_letters(col: Col) -> String {
    let mut n = col as i64 + 1; // 1-based for the bijective base-26
    let mut out = Vec::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        out.push(b'A' + rem);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Parse spreadsheet column letters (case-insensitive) into a zero-based
/// index. Returns `None` for empty/non-alphabetic input.
pub fn letters_to_col(s: &str) -> Option<Col> {
    if s.is_empty() {
        return None;
    }
    let mut n: i64 = 0;
    for ch in s.chars() {
        let up = ch.to_ascii_uppercase();
        if !up.is_ascii_uppercase() {
            return None;
        }
        n = n * 26 + (up as i64 - 'A' as i64 + 1);
    }
    Some((n - 1) as Col)
}

/// Format an address as an A1 string: `(0, 0) -> "A1"`.
pub fn addr_to_a1(col: Col, row: Row) -> String {
    format!("{}{}", col_to_letters(col), row + 1)
}

/// Parse an A1-style reference into a zero-based `(col, row)`.
/// Tolerates absolute markers (`$A$1`, `$A1`, `A$1`). Returns `None`
/// if the syntax doesn't match `[$]LETTERS[$]DIGITS`.
pub fn a1_to_addr(s: &str) -> Option<Addr> {
    let s = s.trim();
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && bytes[i] == b'$' {
        i += 1;
    }
    let letter_start = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == letter_start {
        return None;
    }
    let letters = &s[letter_start..i];
    if i < bytes.len() && bytes[i] == b'$' {
        i += 1;
    }
    let digit_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digit_start || i != bytes.len() {
        return None;
    }
    let digits = &s[digit_start..i];
    let col = letters_to_col(letters)?;
    let row: u32 = digits.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((col, row - 1))
}

/// A fully-resolved cell value. The grid renders these; the formula
/// engine produces them. Raw user input is stored separately as a
/// string and only becomes a `CellValue` once parsed/evaluated.
#[derive(Clone, Debug, PartialEq)]
pub enum CellValue {
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    /// A spreadsheet error like `#REF!`, `#DIV/0!`, `#CYCLE!`, `#ERR!`.
    Error(String),
}

impl CellValue {
    /// Coerce to a number for arithmetic. Text that parses as a number
    /// is accepted; booleans map to 1/0; empty maps to 0. Non-numeric
    /// text yields `None` (caller decides whether that's a `#VALUE!`).
    pub fn as_number(&self) -> Option<f64> {
        match self {
            CellValue::Number(n) => Some(*n),
            CellValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            CellValue::Empty => Some(0.0),
            CellValue::Text(t) => t.trim().parse::<f64>().ok(),
            CellValue::Error(_) => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, CellValue::Error(_))
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, CellValue::Empty)
    }

    /// Plain-text rendering with no format applied (used by the formula
    /// engine for `&` concatenation and by CSV export fallbacks).
    pub fn to_plain_string(&self) -> String {
        match self {
            CellValue::Empty => String::new(),
            CellValue::Number(n) => format_number_general(*n),
            CellValue::Text(t) => t.clone(),
            CellValue::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
            CellValue::Error(e) => e.clone(),
        }
    }

    /// Whether this value should render right-aligned (numbers/bools/dates).
    pub fn is_numeric(&self) -> bool {
        matches!(self, CellValue::Number(_) | CellValue::Bool(_))
    }
}

/// Per-cell display format. The raw input and computed value are
/// format-agnostic; this only governs how the value is rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CellFormat {
    /// Auto: integers show plain, fractions show a few significant digits.
    #[default]
    General,
    /// Fixed two-decimal with thousands separators (`1,234.50`).
    Number,
    /// Currency (`$1,234.50`).
    Currency,
    /// Percentage (`12.50%`) — the stored number is multiplied by 100.
    Percent,
    /// Date — the number is read as an Excel serial (days since
    /// 1899-12-30) and shown `YYYY-MM-DD`.
    Date,
    /// Force text rendering even for numeric content.
    Text,
}

impl CellFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            CellFormat::General => "general",
            CellFormat::Number => "number",
            CellFormat::Currency => "currency",
            CellFormat::Percent => "percent",
            CellFormat::Date => "date",
            CellFormat::Text => "text",
        }
    }

    pub fn from_str(s: &str) -> CellFormat {
        match s {
            "number" => CellFormat::Number,
            "currency" => CellFormat::Currency,
            "percent" => CellFormat::Percent,
            "date" => CellFormat::Date,
            "text" => CellFormat::Text,
            _ => CellFormat::General,
        }
    }

    /// Short label for the toolbar format chip.
    pub fn label(self) -> &'static str {
        match self {
            CellFormat::General => "General",
            CellFormat::Number => "Number",
            CellFormat::Currency => "Currency",
            CellFormat::Percent => "Percent",
            CellFormat::Date => "Date",
            CellFormat::Text => "Text",
        }
    }
}

/// Render a value through a format into the final on-screen string.
pub fn format_value(value: &CellValue, fmt: CellFormat) -> String {
    match value {
        CellValue::Empty => String::new(),
        CellValue::Error(e) => e.clone(),
        CellValue::Text(t) => t.clone(),
        CellValue::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        CellValue::Number(n) => match fmt {
            CellFormat::General => format_number_general(*n),
            CellFormat::Text => format_number_general(*n),
            CellFormat::Number => format_thousands(*n, 2, ""),
            CellFormat::Currency => format_thousands(*n, 2, "$"),
            CellFormat::Percent => format!("{:.2}%", *n * 100.0),
            CellFormat::Date => serial_to_date(*n),
        },
    }
}

/// "General"-style number rendering: integers print without a decimal
/// point, fractions print with trailing zeros trimmed (up to 10 sig
/// figures), and very large/small magnitudes fall back to a compact
/// representation.
pub fn format_number_general(n: f64) -> String {
    if n == 0.0 {
        return "0".to_string();
    }
    if !n.is_finite() {
        return if n.is_nan() { "#NUM!".into() } else if n > 0.0 { "∞".into() } else { "-∞".into() };
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    // Trim trailing zeros from a fixed-precision rendering.
    let s = format!("{:.10}", n);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

/// Fixed-decimal rendering with grouped thousands and an optional prefix
/// (e.g. a currency symbol). Negative values get a leading `-` after the
/// prefix: `-$1,200.00`.
pub fn format_thousands(n: f64, decimals: usize, prefix: &str) -> String {
    let neg = n < 0.0;
    let abs = n.abs();
    let s = format!("{:.*}", decimals, abs);
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s.clone(), None),
    };
    // Group the integer part into threes.
    let mut grouped = String::new();
    let digits: Vec<char> = int_part.chars().collect();
    let len = digits.len();
    for (idx, ch) in digits.iter().enumerate() {
        if idx > 0 && (len - idx) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(*ch);
    }
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(prefix);
    out.push_str(&grouped);
    if let Some(f) = frac_part {
        out.push('.');
        out.push_str(&f);
    }
    out
}

/// Convert an Excel-style serial date (days since 1899-12-30) into a
/// `YYYY-MM-DD` string. Uses a civil-calendar algorithm (Howard Hinnant's
/// days_from_civil inverse) so it's correct across centuries without a
/// chrono dependency.
pub fn serial_to_date(serial: f64) -> String {
    // Excel epoch 1899-12-30 corresponds to Unix day -25569.
    let days = serial.floor() as i64 - 25569;
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Inverse of days_from_civil: days since Unix epoch -> (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_letters_round_trip() {
        assert_eq!(col_to_letters(0), "A");
        assert_eq!(col_to_letters(25), "Z");
        assert_eq!(col_to_letters(26), "AA");
        assert_eq!(col_to_letters(701), "ZZ");
        assert_eq!(letters_to_col("A"), Some(0));
        assert_eq!(letters_to_col("Z"), Some(25));
        assert_eq!(letters_to_col("AA"), Some(26));
        assert_eq!(letters_to_col("ZZ"), Some(701));
    }

    #[test]
    fn a1_parses_with_absolute_markers() {
        assert_eq!(a1_to_addr("A1"), Some((0, 0)));
        assert_eq!(a1_to_addr("$A$1"), Some((0, 0)));
        assert_eq!(a1_to_addr("B3"), Some((1, 2)));
        assert_eq!(a1_to_addr("AA10"), Some((26, 9)));
        assert_eq!(a1_to_addr("A0"), None);
        assert_eq!(a1_to_addr("1A"), None);
        assert_eq!(a1_to_addr(""), None);
    }

    #[test]
    fn thousands_grouping() {
        assert_eq!(format_thousands(1234.5, 2, ""), "1,234.50");
        assert_eq!(format_thousands(-1200.0, 2, "$"), "-$1,200.00");
        assert_eq!(format_thousands(12.0, 0, ""), "12");
    }

    #[test]
    fn general_trims_trailing_zeros() {
        assert_eq!(format_number_general(42.0), "42");
        assert_eq!(format_number_general(3.14), "3.14");
        assert_eq!(format_number_general(0.5), "0.5");
    }

    #[test]
    fn serial_date_known_value() {
        // Excel serial 1 == 1900-01-01 (with the historical leap bug the
        // real Excel epoch is 1899-12-31, but our offset uses 1899-12-30
        // which matches LibreOffice's default). 45000 ~ 2023-03-15.
        let s = serial_to_date(45000.0);
        assert!(s.starts_with("2023-"), "got {s}");
    }
}
