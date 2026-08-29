//! Pure data model for the spreadsheet: cell addressing (A1 notation),
//! cell values, and per-cell number/text/date/currency formats.
//!
//! Nothing here touches the CRDT or the renderer — it's the shared
//! vocabulary the formula engine, the grid widget, and the document
//! layer all speak. Cells are addressed internally by zero-based
//! `(col, row)`; `A1` is `(0, 0)`. The `col_to_letters` / `a1_to_addr`
//! helpers translate to and from the user-facing A1 strings.

use serde::{Deserialize, Serialize};

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

/// A spreadsheet evaluation error, carried as a specific *kind* rather than
/// a free string. Keeping the kind lets callers such as `IFNA`/`IFERROR`
/// branch on the exact cause and guarantees the on-screen spelling is always
/// the canonical Excel code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormulaError {
    /// Division by zero — `#DIV/0!`.
    Div0,
    /// Unknown function or unrecognized name — `#NAME?`.
    Name,
    /// Invalid or broken cell reference — `#REF!`.
    Ref,
    /// Wrong type of argument or operand — `#VALUE!`.
    Value,
    /// A value is not available, e.g. a lookup miss — `#N/A`.
    Na,
    /// Invalid numeric argument or result — `#NUM!`.
    Num,
    /// Circular reference between cells — `#CYCLE!`.
    Cycle,
    /// A formula that could not be parsed — `#ERROR!`.
    Error,
}

impl FormulaError {
    /// The canonical Excel spelling of this error, as shown in the cell.
    pub fn text(self) -> &'static str {
        match self {
            FormulaError::Div0 => "#DIV/0!",
            FormulaError::Name => "#NAME?",
            FormulaError::Ref => "#REF!",
            FormulaError::Value => "#VALUE!",
            FormulaError::Na => "#N/A",
            FormulaError::Num => "#NUM!",
            FormulaError::Cycle => "#CYCLE!",
            FormulaError::Error => "#ERROR!",
        }
    }
}

impl std::fmt::Display for FormulaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
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
    /// A spreadsheet error, rendered as its canonical Excel code
    /// (`#DIV/0!`, `#NAME?`, `#REF!`, `#VALUE!`, `#N/A`, `#NUM!`, `#CYCLE!`).
    Error(FormulaError),
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
            CellValue::Bool(b) => {
                if *b {
                    "TRUE".into()
                } else {
                    "FALSE".into()
                }
            }
            CellValue::Error(e) => e.text().to_string(),
        }
    }

    /// Whether this value should render right-aligned (numbers/bools/dates).
    pub fn is_numeric(&self) -> bool {
        matches!(self, CellValue::Number(_) | CellValue::Bool(_))
    }
}

/// A specific world currency. Each carries its own symbol, placement, and
/// conventional number of decimals — `$1,234.50` vs `1.234,50 €` vs
/// `¥1,235` (yen has no minor unit).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Currency {
    USD,
    EUR,
    GBP,
    JPY,
    CHF,
    CAD,
    AUD,
    CNY,
    INR,
}

impl Currency {
    /// The currency's display symbol (or ISO code where there's no common
    /// glyph, e.g. CHF).
    pub fn symbol(self) -> &'static str {
        match self {
            Currency::USD | Currency::CAD | Currency::AUD => "$",
            Currency::EUR => "\u{20AC}",                 // €
            Currency::GBP => "\u{00A3}",                 // £
            Currency::JPY | Currency::CNY => "\u{00A5}", // ¥
            Currency::CHF => "CHF",
            Currency::INR => "\u{20B9}", // ₹
        }
    }

    /// ISO 4217 code, used as the stable serialized form.
    pub fn code(self) -> &'static str {
        match self {
            Currency::USD => "USD",
            Currency::EUR => "EUR",
            Currency::GBP => "GBP",
            Currency::JPY => "JPY",
            Currency::CHF => "CHF",
            Currency::CAD => "CAD",
            Currency::AUD => "AUD",
            Currency::CNY => "CNY",
            Currency::INR => "INR",
        }
    }

    pub fn from_code(s: &str) -> Option<Currency> {
        Some(match s.to_ascii_uppercase().as_str() {
            "USD" => Currency::USD,
            "EUR" => Currency::EUR,
            "GBP" => Currency::GBP,
            "JPY" => Currency::JPY,
            "CHF" => Currency::CHF,
            "CAD" => Currency::CAD,
            "AUD" => Currency::AUD,
            "CNY" => Currency::CNY,
            "INR" => Currency::INR,
            _ => return None,
        })
    }

    /// Conventional minor-unit digits: 2 for most, 0 for yen/yuan-as-yen.
    pub fn decimals(self) -> usize {
        match self {
            Currency::JPY => 0,
            _ => 2,
        }
    }

    /// Whether the symbol leads the amount (`$5`) or trails it (`5 €`).
    pub fn symbol_leads(self) -> bool {
        !matches!(self, Currency::EUR | Currency::CHF)
    }

    /// Every currency the picker offers, in display order.
    pub const ALL: [Currency; 9] = [
        Currency::USD,
        Currency::EUR,
        Currency::GBP,
        Currency::JPY,
        Currency::CHF,
        Currency::CAD,
        Currency::AUD,
        Currency::CNY,
        Currency::INR,
    ];

    /// Map a bare currency *symbol* (as typed) to a currency. Ambiguous
    /// symbols (`$`, `¥`) resolve to the most common owner so smart-entry
    /// has a sensible default; the per-document currency can override it.
    pub fn from_symbol(sym: &str) -> Option<Currency> {
        Some(match sym {
            "$" => Currency::USD,
            "\u{20AC}" | "EUR" => Currency::EUR,
            "\u{00A3}" => Currency::GBP,
            "\u{00A5}" => Currency::JPY,
            "CHF" => Currency::CHF,
            "\u{20B9}" => Currency::INR,
            _ => return None,
        })
    }
}

/// How dates render. The serde names match the codes used in the shared
/// settings file (`ISO`/`US`/`EU`/`Long`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateStyle {
    /// `2023-03-15`
    #[serde(rename = "ISO")]
    Iso,
    /// `03/15/2023`
    #[serde(rename = "US")]
    Us,
    /// `15/03/2023`
    #[serde(rename = "EU")]
    Eu,
    /// `15 Mar 2023`
    #[serde(rename = "Long")]
    Long,
}

/// How times render. The serde names match the shared-settings codes
/// (`H24`/`H12`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeStyle {
    /// `14:05`
    H24,
    /// `2:05 PM`
    H12,
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
    /// Currency in a specific currency (`$1,234.50`, `1.234,50 €`, `¥1,235`).
    Currency(Currency),
    /// Percentage (`12.50%`) — the stored number is multiplied by 100.
    Percent,
    /// Date — the number is read as an Excel serial (days since
    /// 1899-12-30) and shown in the configured date style.
    Date(DateStyle),
    /// Time — the fractional part of an Excel serial as a clock time.
    Time(TimeStyle),
    /// Boolean — renders `TRUE`/`FALSE`.
    Boolean,
    /// Force text rendering even for numeric content.
    Text,
}

impl CellFormat {
    /// Serialize to the stable string stored in the CRDT. Subtype carriers
    /// encode as `currency:USD`, `date:ISO`, `time:H24`.
    pub fn as_str(self) -> String {
        match self {
            CellFormat::General => "general".into(),
            CellFormat::Number => "number".into(),
            CellFormat::Currency(c) => format!("currency:{}", c.code()),
            CellFormat::Percent => "percent".into(),
            CellFormat::Date(s) => format!("date:{}", date_style_code(s)),
            CellFormat::Time(s) => format!("time:{}", time_style_code(s)),
            CellFormat::Boolean => "boolean".into(),
            CellFormat::Text => "text".into(),
        }
    }

    /// Parse the stored string back into a format. Unknown subtypes fall
    /// back to a default (USD currency, ISO date, 24h time) so an
    /// unrecognized variant never loses the *category*. The supplied
    /// `default_currency` seeds a bare `"currency"` with no subtype.
    pub fn from_str_with(s: &str, default_currency: Currency) -> CellFormat {
        let (head, sub) = match s.split_once(':') {
            Some((h, t)) => (h, Some(t)),
            None => (s, None),
        };
        match head {
            "number" => CellFormat::Number,
            "currency" => CellFormat::Currency(
                sub.and_then(Currency::from_code)
                    .unwrap_or(default_currency),
            ),
            "percent" => CellFormat::Percent,
            "date" => {
                CellFormat::Date(sub.and_then(date_style_from_code).unwrap_or(DateStyle::Iso))
            }
            "time" => {
                CellFormat::Time(sub.and_then(time_style_from_code).unwrap_or(TimeStyle::H24))
            }
            "boolean" => CellFormat::Boolean,
            "text" => CellFormat::Text,
            _ => CellFormat::General,
        }
    }

    /// Convenience parse with the USD/ISO/24h defaults — used where the
    /// document currency isn't handy (e.g. tests).
    pub fn from_str(s: &str) -> CellFormat {
        Self::from_str_with(s, Currency::USD)
    }

    /// The high-level category label shown in the type indicator. For
    /// currency this includes the currency code, e.g. `Currency (EUR)`.
    pub fn label(self) -> String {
        match self {
            CellFormat::General => "General".into(),
            CellFormat::Number => "Number".into(),
            CellFormat::Currency(c) => format!("Currency ({})", c.code()),
            CellFormat::Percent => "Percent".into(),
            CellFormat::Date(_) => "Date".into(),
            CellFormat::Time(_) => "Time".into(),
            CellFormat::Boolean => "Boolean".into(),
            CellFormat::Text => "Text".into(),
        }
    }

    /// The bare category, ignoring subtype — used to highlight the matching
    /// toolbar/menu chip regardless of which currency/date style is active.
    pub fn category(self) -> FormatCategory {
        match self {
            CellFormat::General => FormatCategory::General,
            CellFormat::Number => FormatCategory::Number,
            CellFormat::Currency(_) => FormatCategory::Currency,
            CellFormat::Percent => FormatCategory::Percent,
            CellFormat::Date(_) => FormatCategory::Date,
            CellFormat::Time(_) => FormatCategory::Time,
            CellFormat::Boolean => FormatCategory::Boolean,
            CellFormat::Text => FormatCategory::Text,
        }
    }
}

/// Subtype-agnostic format category, for UI highlighting and propagation
/// decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatCategory {
    General,
    Number,
    Currency,
    Percent,
    Date,
    Time,
    Boolean,
    Text,
}

fn date_style_code(s: DateStyle) -> &'static str {
    match s {
        DateStyle::Iso => "ISO",
        DateStyle::Us => "US",
        DateStyle::Eu => "EU",
        DateStyle::Long => "Long",
    }
}
fn date_style_from_code(s: &str) -> Option<DateStyle> {
    Some(match s {
        "ISO" => DateStyle::Iso,
        "US" => DateStyle::Us,
        "EU" => DateStyle::Eu,
        "Long" => DateStyle::Long,
        _ => return None,
    })
}
fn time_style_code(s: TimeStyle) -> &'static str {
    match s {
        TimeStyle::H24 => "H24",
        TimeStyle::H12 => "H12",
    }
}
fn time_style_from_code(s: &str) -> Option<TimeStyle> {
    Some(match s {
        "H24" => TimeStyle::H24,
        "H12" => TimeStyle::H12,
        _ => return None,
    })
}

/// Render a value through a format into the final on-screen string.
pub fn format_value(value: &CellValue, fmt: CellFormat) -> String {
    match value {
        CellValue::Empty => String::new(),
        CellValue::Error(e) => e.text().to_string(),
        CellValue::Text(t) => t.clone(),
        CellValue::Bool(b) => {
            if *b {
                "TRUE".into()
            } else {
                "FALSE".into()
            }
        }
        CellValue::Number(n) => match fmt {
            CellFormat::General => format_number_general(*n),
            CellFormat::Text => format_number_general(*n),
            CellFormat::Number => format_thousands(*n, 2),
            CellFormat::Currency(c) => format_currency(*n, c),
            CellFormat::Percent => format!("{:.2}%", *n * 100.0),
            CellFormat::Date(s) => serial_to_date(*n, s),
            CellFormat::Time(s) => serial_to_time(*n, s),
            CellFormat::Boolean => {
                if *n != 0.0 {
                    "TRUE".into()
                } else {
                    "FALSE".into()
                }
            }
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
        return if n.is_nan() {
            "#NUM!".into()
        } else if n > 0.0 {
            "∞".into()
        } else {
            "-∞".into()
        };
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    // Trim trailing zeros from a fixed-precision rendering.
    let s = format!("{:.10}", n);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

/// Fixed-decimal rendering with grouped thousands (`1,234.50`). Negative
/// values get a leading `-`.
pub fn format_thousands(n: f64, decimals: usize) -> String {
    let neg = n < 0.0;
    let (int_grouped, frac) = grouped_parts(n.abs(), decimals, ',');
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(&int_grouped);
    if let Some(f) = frac {
        out.push('.');
        out.push_str(&f);
    }
    out
}

/// Render an amount in a specific currency, honoring its symbol, decimals,
/// and placement: `$1,234.50`, `1.234,50 €`, `¥1,235`, `CHF 1'234.50`-ish
/// (we keep `'` out for simplicity and use the Anglo grouping there).
pub fn format_currency(n: f64, c: Currency) -> String {
    let neg = n < 0.0;
    // The euro convention groups with '.' and decimal-separates with ','.
    let (group_sep, dec_sep) = match c {
        Currency::EUR => ('.', ','),
        _ => (',', '.'),
    };
    let (int_grouped, frac) = grouped_parts(n.abs(), c.decimals(), group_sep);
    let mut amount = String::new();
    amount.push_str(&int_grouped);
    if let Some(f) = frac {
        amount.push(dec_sep);
        amount.push_str(&f);
    }
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if c.symbol_leads() {
        out.push_str(c.symbol());
        out.push_str(&amount);
    } else {
        out.push_str(&amount);
        out.push(' ');
        out.push_str(c.symbol());
    }
    out
}

/// Split a non-negative number into a grouped integer part and an optional
/// fractional string of `decimals` digits, using `group_sep` between
/// thousands. Rounding is half-away-from-zero (the spreadsheet convention),
/// not Rust's default round-half-to-even.
fn grouped_parts(abs: f64, decimals: usize, group_sep: char) -> (String, Option<String>) {
    // Round half away from zero so e.g. JPY 1234.5 -> 1235, matching Excel.
    let factor = 10f64.powi(decimals as i32);
    let rounded = (abs * factor + 0.5).floor() / factor;
    let s = format!("{:.*}", decimals, rounded);
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s, None),
    };
    let mut grouped = String::new();
    let digits: Vec<char> = int_part.chars().collect();
    let len = digits.len();
    for (idx, ch) in digits.iter().enumerate() {
        if idx > 0 && (len - idx) % 3 == 0 {
            grouped.push(group_sep);
        }
        grouped.push(*ch);
    }
    (grouped, frac_part)
}

/// Convert an Excel-style serial date (days since 1899-12-30) into a string
/// in the chosen style. Uses a civil-calendar algorithm (Howard Hinnant's
/// days_from_civil inverse) so it's correct across centuries without a
/// chrono dependency.
pub fn serial_to_date(serial: f64, style: DateStyle) -> String {
    // Excel epoch 1899-12-30 corresponds to Unix day -25569.
    let days = serial.floor() as i64 - 25569;
    let (y, m, d) = civil_from_days(days);
    match style {
        DateStyle::Iso => format!("{:04}-{:02}-{:02}", y, m, d),
        DateStyle::Us => format!("{:02}/{:02}/{:04}", m, d, y),
        DateStyle::Eu => format!("{:02}/{:02}/{:04}", d, m, y),
        DateStyle::Long => format!("{:02} {} {:04}", d, month_abbrev(m), y),
    }
}

/// Convert the fractional part of an Excel serial into a clock time.
pub fn serial_to_time(serial: f64, style: TimeStyle) -> String {
    let frac = serial.fract().abs();
    let total_secs = (frac * 86400.0).round() as i64;
    let h = (total_secs / 3600) % 24;
    let mi = (total_secs % 3600) / 60;
    match style {
        TimeStyle::H24 => format!("{:02}:{:02}", h, mi),
        TimeStyle::H12 => {
            let suffix = if h < 12 { "AM" } else { "PM" };
            let h12 = match h % 12 {
                0 => 12,
                x => x,
            };
            format!("{}:{:02} {}", h12, mi, suffix)
        }
    }
}

fn month_abbrev(m: u32) -> &'static str {
    const M: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    M.get((m.max(1).min(12) - 1) as usize)
        .copied()
        .unwrap_or("Jan")
}

/// Convert a calendar date to an Excel serial (days since 1899-12-30).
pub fn date_to_serial(y: i64, m: u32, d: u32) -> f64 {
    (days_from_civil(y, m, d) + 25569) as f64
}

/// Split an Excel serial into its `(year, month, day)` calendar parts.
pub fn serial_to_ymd(serial: f64) -> (i64, u32, u32) {
    civil_from_days(serial.floor() as i64 - 25569)
}

/// The weekday of a serial as Excel's default numbering: `1` = Sunday …
/// `7` = Saturday.
pub fn weekday_from_serial(serial: f64) -> u32 {
    let days = serial.floor() as i64 - 25569; // days since 1970-01-01 (a Thursday)
    // Sunday=0 .. Saturday=6, then shift to Excel's 1-based Sunday-first.
    let dow = (((days + 4) % 7) + 7) % 7;
    dow as u32 + 1
}

/// The current instant as an Excel serial (integer days + fractional day).
/// Uses the system clock in UTC — there is no timezone database in this
/// environment, so `TODAY()`/`NOW()` read as UTC dates.
pub fn now_serial() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    secs / 86400.0 + 25569.0
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

/// Days since Unix epoch for a civil date (Howard Hinnant).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
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
        assert_eq!(format_thousands(1234.5, 2), "1,234.50");
        assert_eq!(format_thousands(-1200.0, 2), "-1,200.00");
        assert_eq!(format_thousands(12.0, 0), "12");
    }

    #[test]
    fn general_trims_trailing_zeros() {
        assert_eq!(format_number_general(42.0), "42");
        assert_eq!(format_number_general(3.14), "3.14");
        assert_eq!(format_number_general(0.5), "0.5");
    }

    #[test]
    fn currency_symbol_placement_and_decimals() {
        // USD: leading $, two decimals, comma grouping.
        assert_eq!(format_currency(1234.5, Currency::USD), "$1,234.50");
        assert_eq!(format_currency(-1200.0, Currency::USD), "-$1,200.00");
        // EUR: trailing €, dot grouping, comma decimal.
        assert_eq!(format_currency(1234.5, Currency::EUR), "1.234,50 \u{20AC}");
        // GBP: leading £.
        assert_eq!(format_currency(99.9, Currency::GBP), "\u{00A3}99.90");
        // JPY: leading ¥, no decimals.
        assert_eq!(format_currency(1234.5, Currency::JPY), "\u{00A5}1,235");
        // CHF: trailing code.
        assert_eq!(format_currency(50.0, Currency::CHF), "50.00 CHF");
    }

    #[test]
    fn currency_format_round_trips_through_string() {
        for c in Currency::ALL {
            let fmt = CellFormat::Currency(c);
            let s = fmt.as_str();
            assert_eq!(CellFormat::from_str(&s), fmt, "round trip {s}");
        }
    }

    #[test]
    fn date_styles_render() {
        let serial = date_to_serial(2023, 3, 15);
        assert_eq!(serial_to_date(serial, DateStyle::Iso), "2023-03-15");
        assert_eq!(serial_to_date(serial, DateStyle::Us), "03/15/2023");
        assert_eq!(serial_to_date(serial, DateStyle::Eu), "15/03/2023");
        assert_eq!(serial_to_date(serial, DateStyle::Long), "15 Mar 2023");
    }

    #[test]
    fn date_serial_round_trips() {
        // 45000 ~ 2023-03-15 per the LibreOffice 1899-12-30 epoch.
        let s = serial_to_date(45000.0, DateStyle::Iso);
        assert!(s.starts_with("2023-"), "got {s}");
        let serial = date_to_serial(2023, 3, 15);
        let back = serial_to_date(serial, DateStyle::Iso);
        assert_eq!(back, "2023-03-15");
    }

    #[test]
    fn time_styles_render() {
        // 0.5 of a day == noon.
        assert_eq!(serial_to_time(0.5, TimeStyle::H24), "12:00");
        assert_eq!(serial_to_time(0.5, TimeStyle::H12), "12:00 PM");
        // 14:05 == (14*3600 + 5*60)/86400.
        let serial = (14.0 * 3600.0 + 5.0 * 60.0) / 86400.0;
        assert_eq!(serial_to_time(serial, TimeStyle::H24), "14:05");
        assert_eq!(serial_to_time(serial, TimeStyle::H12), "2:05 PM");
    }

    #[test]
    fn format_string_round_trip_all_variants() {
        let cases = [
            CellFormat::General,
            CellFormat::Number,
            CellFormat::Percent,
            CellFormat::Boolean,
            CellFormat::Text,
            CellFormat::Currency(Currency::EUR),
            CellFormat::Date(DateStyle::Eu),
            CellFormat::Time(TimeStyle::H12),
        ];
        for f in cases {
            assert_eq!(CellFormat::from_str(&f.as_str()), f);
        }
    }
}
