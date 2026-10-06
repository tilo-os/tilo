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

/// One inserted or deleted row/column: the unit every structural action
/// replays in (a multi-row delete applies one row at a time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridEdit {
    /// A blank row inserted above this row.
    InsertRow(Row),
    DeleteRow(Row),
    /// A blank column inserted left of this column.
    InsertColumn(Col),
    DeleteColumn(Col),
}

impl GridEdit {
    /// Where the cell at `addr` lands; `None` when its row/column is deleted.
    pub fn map_addr(self, (col, row): Addr) -> Option<Addr> {
        match self {
            Self::InsertRow(at) => Some((col, insert_shift(row, at))),
            Self::DeleteRow(at) => Some((col, delete_shift(row, at)?)),
            Self::InsertColumn(at) => Some((insert_shift(col, at), row)),
            Self::DeleteColumn(at) => Some((delete_shift(col, at)?, row)),
        }
    }

    /// Where the rectangle with corners `a` and `b` (in either order) lands,
    /// each corner keeping its role. Mapping the corners independently is
    /// wrong: deleting a range's first or last line shrinks the range, and
    /// only deleting every line it covers removes it (`None`).
    pub fn map_range(self, a: Addr, b: Addr) -> Option<(Addr, Addr)> {
        match self {
            Self::InsertRow(_) | Self::InsertColumn(_) => {
                Some((self.map_addr(a)?, self.map_addr(b)?))
            }
            Self::DeleteRow(at) => {
                let (ra, rb) = delete_span(a.1, b.1, at)?;
                Some(((a.0, ra), (b.0, rb)))
            }
            Self::DeleteColumn(at) => {
                let (ca, cb) = delete_span(a.0, b.0, at)?;
                Some(((ca, a.1), (cb, b.1)))
            }
        }
    }
}

fn insert_shift(line: u32, at: u32) -> u32 {
    if line >= at {
        line.saturating_add(1)
    } else {
        line
    }
}

fn delete_shift(line: u32, at: u32) -> Option<u32> {
    match line.cmp(&at) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some(line.saturating_sub(1)),
        std::cmp::Ordering::Less => Some(line),
    }
}

/// Map a span's two ends (in either order) through deleting line `at`.
fn delete_span(a: u32, b: u32, at: u32) -> Option<(u32, u32)> {
    let (lo, hi) = (a.min(b), a.max(b));
    if lo == at && hi == at {
        return None;
    }
    // A deleted first line is replaced by the next one sliding up into it.
    let new_lo = if lo > at { lo.saturating_sub(1) } else { lo };
    let new_hi = if hi >= at { hi.saturating_sub(1) } else { hi };
    Some(if a <= b {
        (new_lo, new_hi)
    } else {
        (new_hi, new_lo)
    })
}

/// Convert a zero-based column index to spreadsheet letters.
/// `0 -> "A"`, `25 -> "Z"`, `26 -> "AA"`, `701 -> "ZZ"`. The letters are
/// qdoc's, the same the `.xlsx`/`.ods` codecs write.
pub fn col_to_letters(col: Col) -> String {
    tilo_doc::sheet::a1::index_to_col(col.saturating_add(1))
}

/// Parse spreadsheet column letters (case-insensitive) into a zero-based
/// index. Returns `None` for empty or non-alphabetic input, and for a run of
/// more than three letters (`XFD`, the last column, has three).
pub fn letters_to_col(s: &str) -> Option<Col> {
    tilo_doc::sheet::a1::col_to_index(s)?.checked_sub(1)
}

/// Format an address as an A1 string: `(0, 0) -> "A1"`.
pub fn addr_to_a1(col: Col, row: Row) -> String {
    // Rows read one-based on screen; the very last row saturates rather than
    // wrapping around to row 1.
    format!("{}{}", col_to_letters(col), row.saturating_add(1))
}

/// Parse an A1-style reference into a zero-based `(col, row)`.
/// Tolerates absolute markers (`$A$1`, `$A1`, `A$1`). Returns `None`
/// if the syntax doesn't match `[$]LETTERS[$]DIGITS`.
pub fn a1_to_addr(s: &str) -> Option<Addr> {
    let s = s.trim();
    let after_col_anchor = s.strip_prefix('$').unwrap_or(s);
    // ASCII letters are one byte each, so the count is also the byte offset.
    let letters_len = after_col_anchor
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .count();
    let (letters, rest) = after_col_anchor.split_at_checked(letters_len)?;
    if letters.is_empty() {
        return None;
    }
    let digits = rest.strip_prefix('$').unwrap_or(rest);
    // Anything left that is not a digit (a stray suffix, another `$`) means
    // this was never an A1 reference.
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let col = letters_to_col(letters)?;
    let row: Row = digits.parse().ok()?;
    // Rows are one-based on screen, so there is no row 0.
    Some((col, row.checked_sub(1)?))
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
    #[allow(
        clippy::should_implement_trait,
        reason = "parsing cannot fail (an unknown code reads as `General`); `FromStr` would hand every caller a `Result` to unwrap for no gain"
    )]
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
            CellFormat::General | CellFormat::Text => format_number_general(*n),
            CellFormat::Number => format_thousands(*n, 2),
            CellFormat::Currency(c) => format_currency(*n, c),
            CellFormat::Percent => format!("{:.2}%", *n * 100.0),
            CellFormat::Date(s) => serial_to_date(*n, s),
            CellFormat::Time(s) => serial_to_time(*n, s),
            CellFormat::Boolean => {
                if *n == 0.0 {
                    "FALSE".into()
                } else {
                    "TRUE".into()
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
        // Whole and inside the exactly-representable range: print the integer
        // straight, with no decimal point.
        return format!("{n:.0}");
    }
    // Trim trailing zeros from a fixed-precision rendering.
    let s = format!("{n:.10}");
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
    // Past f64's ~17 significant digits more decimals say nothing, and the
    // bound keeps the `powi` exponent inside `i32`.
    let decimals = decimals.min(17);
    let exponent = i32::try_from(decimals).unwrap_or(17);
    // Round half away from zero so e.g. JPY 1234.5 -> 1235, matching Excel.
    let factor = 10f64.powi(exponent);
    let rounded = (abs * factor + 0.5).floor() / factor;
    let s = format!("{rounded:.decimals$}");
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s, None),
    };
    let mut grouped = String::new();
    let len = int_part.chars().count();
    for (idx, ch) in int_part.chars().enumerate() {
        // `enumerate` keeps `idx < len`, so this never saturates: it is how
        // many digits still follow the one being pushed.
        let remaining = len.saturating_sub(idx);
        if idx > 0 && remaining.is_multiple_of(3) {
            grouped.push(group_sep);
        }
        grouped.push(ch);
    }
    (grouped, frac_part)
}

/// Days since the Unix epoch for an Excel serial (days since 1899-12-30).
///
/// A cell can hold any number at all, so the serial is first bounded to
/// roughly the years -9000..13000. That is far outside any date a spreadsheet
/// means, and it keeps the calendar arithmetic downstream nowhere near the
/// edges of `i64`. A non-finite serial reads as the epoch.
fn serial_to_unix_days(serial: f64) -> i64 {
    // Excel epoch 1899-12-30 corresponds to Unix day -25569.
    const EXCEL_EPOCH_UNIX_DAY: i64 = -25569;
    let bounded = serial.floor().clamp(-4_000_000.0, 4_000_000.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded to ±4e6 on the line above, so the truncation is exact; a NaN serial converts to 0 and reads as the Excel epoch"
    )]
    let days = bounded as i64;
    days.saturating_add(EXCEL_EPOCH_UNIX_DAY)
}

/// Convert an Excel-style serial date (days since 1899-12-30) into a string
/// in the chosen style. Uses a civil-calendar algorithm (Howard Hinnant's
/// `days_from_civil` inverse) so it's correct across centuries without a
/// chrono dependency.
pub fn serial_to_date(serial: f64, style: DateStyle) -> String {
    let (y, m, d) = civil_from_days(serial_to_unix_days(serial));
    match style {
        DateStyle::Iso => format!("{y:04}-{m:02}-{d:02}"),
        DateStyle::Us => format!("{m:02}/{d:02}/{y:04}"),
        DateStyle::Eu => format!("{d:02}/{m:02}/{y:04}"),
        DateStyle::Long => format!("{:02} {} {:04}", d, month_abbrev(m), y),
    }
}

/// Convert the fractional part of an Excel serial into a clock time.
pub fn serial_to_time(serial: f64, style: TimeStyle) -> String {
    let frac = serial.fract().abs();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "`frac` is a fractional part in [0, 1), so the product is at most 86400 and the truncation is exact"
    )]
    let total_secs = (frac * 86400.0).round() as i64;
    let h = (total_secs / 3600) % 24;
    let mi = (total_secs % 3600) / 60;
    match style {
        TimeStyle::H24 => format!("{h:02}:{mi:02}"),
        TimeStyle::H12 => {
            let suffix = if h < 12 { "AM" } else { "PM" };
            let h12 = match h % 12 {
                0 => 12,
                x => x,
            };
            format!("{h12}:{mi:02} {suffix}")
        }
    }
}

fn month_abbrev(m: u32) -> &'static str {
    const M: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let idx = m.clamp(1, 12).saturating_sub(1);
    usize::try_from(idx)
        .ok()
        .and_then(|i| M.get(i))
        .copied()
        .unwrap_or("Jan")
}

/// Convert a calendar date to an Excel serial (days since 1899-12-30).
pub fn date_to_serial(y: i64, m: u32, d: u32) -> f64 {
    let days = days_from_civil(y, m, d).saturating_add(25569);
    // A day count outside `i32` is already millions of years from any real
    // date; narrowing there keeps the widening to f64 exact rather than
    // silently rounding a 64-bit day number.
    let days = i32::try_from(days).unwrap_or(if days < 0 { i32::MIN } else { i32::MAX });
    f64::from(days)
}

/// Split an Excel serial into its `(year, month, day)` calendar parts.
pub fn serial_to_ymd(serial: f64) -> (i64, u32, u32) {
    civil_from_days(serial_to_unix_days(serial))
}

/// The weekday of a serial as Excel's default numbering: `1` = Sunday …
/// `7` = Saturday.
pub fn weekday_from_serial(serial: f64) -> u32 {
    let days = serial_to_unix_days(serial); // days since 1970-01-01 (a Thursday)
    // Sunday=0 .. Saturday=6, then shift to Excel's 1-based Sunday-first.
    let dow = days.saturating_add(4).rem_euclid(7);
    u32::try_from(dow).unwrap_or(0).saturating_add(1)
}

/// The current local date and time as an Excel serial (integer days +
/// fractional day), read through the toolkit clock so a capture shows the
/// same `TODAY()` and `NOW()` on every run.
pub fn now_serial() -> f64 {
    let local = ui::clock::local_now().naive_local().and_utc();
    let secs = local.timestamp() as f64 + f64::from(local.timestamp_subsec_millis()) / 1000.0;
    secs / 86400.0 + 25569.0
}

/// Inverse of `days_from_civil`: days since Unix epoch -> (year, month, day).
/// Bounded to ±4 000 000 days (years ~ -9000..13000), the range a serial date
/// can mean.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    ui::date::civil_from_days(z.clamp(-4_000_000, 4_000_000))
}

/// Days since Unix epoch for a civil date. A year outside ±10 000 000 is not a
/// date any document can mean, so it is bounded first.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    ui::date::days_from_civil(y.clamp(-10_000_000, 10_000_000), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Column letters, A1 parsing, grouping and General trimming.
    #[test]
    #[allow(
        clippy::approx_constant,
        reason = "3.14 is the value under test, not an approximation of PI"
    )]
    fn cell_addresses_and_number_formatting() {
        // Case `column_letters_round_trip`.
        {
            assert_eq!(col_to_letters(0), "A");
            assert_eq!(col_to_letters(25), "Z");
            assert_eq!(col_to_letters(26), "AA");
            assert_eq!(col_to_letters(701), "ZZ");
            assert_eq!(letters_to_col("A"), Some(0));
            assert_eq!(letters_to_col("Z"), Some(25));
            assert_eq!(letters_to_col("AA"), Some(26));
            assert_eq!(letters_to_col("ZZ"), Some(701));
            assert_eq!(letters_to_col("XFD"), Some(16_383));
            // Past the last column a run of letters is a name, not a column.
            assert_eq!(letters_to_col("ABCD"), None);
        }

        // Case `a1_parses_with_absolute_markers`.
        {
            assert_eq!(a1_to_addr("A1"), Some((0, 0)));
            assert_eq!(a1_to_addr("$A$1"), Some((0, 0)));
            assert_eq!(a1_to_addr("B3"), Some((1, 2)));
            assert_eq!(a1_to_addr("AA10"), Some((26, 9)));
            assert_eq!(a1_to_addr("A0"), None);
            assert_eq!(a1_to_addr("1A"), None);
            assert_eq!(a1_to_addr(""), None);
        }

        // Case `thousands_grouping`.
        {
            assert_eq!(format_thousands(1234.5, 2), "1,234.50");
            assert_eq!(format_thousands(-1200.0, 2), "-1,200.00");
            assert_eq!(format_thousands(12.0, 0), "12");
        }

        // Case `general_trims_trailing_zeros`.
        {
            assert_eq!(format_number_general(42.0), "42");
            assert_eq!(format_number_general(3.14), "3.14");
            assert_eq!(format_number_general(0.5), "0.5");
        }
    }

    /// Currency, date and time formats and format-string round-trips.
    #[test]
    fn currency_date_and_time_formats_render_and_round_trip() {
        // Case `currency_symbol_placement_and_decimals`.
        {
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

        // Case `currency_format_round_trips_through_string`.
        {
            for c in Currency::ALL {
                let fmt = CellFormat::Currency(c);
                let s = fmt.as_str();
                assert_eq!(CellFormat::from_str(&s), fmt, "round trip {s}");
            }
        }

        // Case `date_styles_render`.
        {
            let serial = date_to_serial(2023, 3, 15);
            assert_eq!(serial_to_date(serial, DateStyle::Iso), "2023-03-15");
            assert_eq!(serial_to_date(serial, DateStyle::Us), "03/15/2023");
            assert_eq!(serial_to_date(serial, DateStyle::Eu), "15/03/2023");
            assert_eq!(serial_to_date(serial, DateStyle::Long), "15 Mar 2023");
        }

        // Case `date_serial_round_trips`.
        {
            // 45000 ~ 2023-03-15 per the LibreOffice 1899-12-30 epoch.
            let s = serial_to_date(45000.0, DateStyle::Iso);
            assert!(s.starts_with("2023-"), "got {s}");
            let serial = date_to_serial(2023, 3, 15);
            let back = serial_to_date(serial, DateStyle::Iso);
            assert_eq!(back, "2023-03-15");
        }

        // Case `time_styles_render`.
        {
            // 0.5 of a day == noon.
            assert_eq!(serial_to_time(0.5, TimeStyle::H24), "12:00");
            assert_eq!(serial_to_time(0.5, TimeStyle::H12), "12:00 PM");
            // 14:05 == (14*3600 + 5*60)/86400.
            let serial = (14.0 * 3600.0 + 5.0 * 60.0) / 86400.0;
            assert_eq!(serial_to_time(serial, TimeStyle::H24), "14:05");
            assert_eq!(serial_to_time(serial, TimeStyle::H12), "2:05 PM");
        }

        // Case `format_string_round_trip_all_variants`.
        {
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
}
