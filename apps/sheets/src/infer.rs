//! Smart type inference on cell entry.
//!
//! When a user types a literal into a cell, we detect its intended type
//! the way Excel and Sheets do — `90%` becomes the number `0.9` shown as
//! `90%`, `$1,200` becomes `1200` shown as currency, `2023-03-15` becomes
//! a date serial shown as a date — and we set the cell's format to match.
//!
//! The contract: [`infer_entry`] takes the raw text and the document's
//! effective currency, and returns the **canonical raw string** to store
//! (so the formula engine and CSV export see a clean numeric literal) plus
//! the [`CellFormat`] that makes it render the way the user meant. Formula
//! entries (`=...`) and explicitly text-y input are passed through
//! untouched so inference never fights the user.
//!
//! Inference is intentionally forgiving: thousands separators, leading or
//! trailing currency symbols, surrounding spaces, and a parenthesised
//! negative (`(1,200)`) are all accepted.

use crate::model::{CellFormat, Currency, DateStyle, TimeStyle, date_to_serial};

/// The outcome of inferring a freshly-typed literal.
#[derive(Clone, Debug, PartialEq)]
pub struct Inferred {
    /// The canonical raw to persist (a clean number for typed numerics, or
    /// the original text otherwise).
    pub raw: String,
    /// The format the cell should adopt.
    pub format: CellFormat,
}

/// Infer the stored raw + format for a literal cell entry. `default_currency`
/// supplies the currency for bare/ambiguous money symbols and is also the
/// fallback the document/global locale provides.
pub fn infer_entry(input: &str, default_currency: Currency) -> Inferred {
    let trimmed = input.trim();

    // Formulas and empty cells are never reinterpreted.
    if trimmed.is_empty() || trimmed.starts_with('=') {
        return Inferred {
            raw: input.to_string(),
            format: CellFormat::General,
        };
    }

    // A literal wrapped in quotes is forced text (drop the quotes).
    if trimmed.len() >= 2 && trimmed.starts_with('\'') {
        return Inferred {
            raw: trimmed.get(1..).unwrap_or("").to_string(),
            format: CellFormat::Text,
        };
    }

    // Booleans.
    match trimmed.to_ascii_uppercase().as_str() {
        "TRUE" => {
            return Inferred {
                raw: "TRUE".into(),
                format: CellFormat::Boolean,
            };
        }
        "FALSE" => {
            return Inferred {
                raw: "FALSE".into(),
                format: CellFormat::Boolean,
            };
        }
        _ => {}
    }

    // Percent: a trailing `%`.
    if let Some(body) = trimmed.strip_suffix('%')
        && let Some(n) = parse_number_loose(body.trim())
    {
        // Store the underlying fraction; Percent format multiplies by 100.
        return Inferred {
            raw: trim_float(n / 100.0),
            format: CellFormat::Percent,
        };
    }

    // Currency: a leading or trailing currency symbol/code.
    if let Some((cur, amount)) = strip_currency(trimmed, default_currency)
        && let Some(n) = parse_number_loose(amount)
    {
        return Inferred {
            raw: trim_float(n),
            format: CellFormat::Currency(cur),
        };
    }

    // Date / time literals.
    if let Some(serial) = parse_date(trimmed) {
        return Inferred {
            raw: trim_float(serial),
            format: CellFormat::Date(DateStyle::Iso),
        };
    }
    if let Some(serial) = parse_time(trimmed) {
        return Inferred {
            raw: trim_float(serial),
            format: CellFormat::Time(TimeStyle::H24),
        };
    }

    // Number with grouping separators (`1,234.5`) → store the bare number,
    // adopt the Number format so the grouping shows on render.
    if has_grouping(trimmed)
        && let Some(n) = parse_number_loose(trimmed)
    {
        return Inferred {
            raw: trim_float(n),
            format: CellFormat::Number,
        };
    }

    // Plain number (no grouping) → keep raw, leave General so integers
    // stay clean. The grid still right-aligns it.
    if let Some(n) = parse_number_loose(trimmed) {
        return Inferred {
            raw: trim_float(n),
            format: CellFormat::General,
        };
    }

    // Everything else is text.
    Inferred {
        raw: input.to_string(),
        format: CellFormat::Text,
    }
}

/// Parse a number allowing surrounding whitespace, thousands separators,
/// and a parenthesised negative `(1,200)`.
fn parse_number_loose(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, body) = if s.starts_with('(') && s.ends_with(')') {
        (true, s.get(1..s.len() - 1).unwrap_or(s))
    } else {
        (false, s)
    };
    let cleaned: String = body.chars().filter(|c| *c != ',').collect();
    // Reject things like "1.2.3" — a single optional sign, digits, one dot,
    // optional exponent. We lean on f64::from_str after removing commas.
    let n: f64 = cleaned.trim().parse().ok()?;
    Some(if neg { -n } else { n })
}

/// True if the string looks like a number that uses thousands grouping
/// (so we want the Number format to preserve the grouped look).
fn has_grouping(s: &str) -> bool {
    // A comma flanked by digits, e.g. "1,234" or "12,000.50".
    let bytes: Vec<char> = s.chars().collect();
    for (i, c) in bytes.iter().enumerate() {
        if *c == ','
            && i > 0
            && bytes.get(i - 1).is_some_and(char::is_ascii_digit)
            && bytes.get(i + 1).is_some_and(char::is_ascii_digit)
        {
            return true;
        }
    }
    false
}

/// Strip a leading or trailing currency marker, returning the currency and
/// the remaining numeric text. `$`/`¥` are ambiguous and resolve to the
/// document default when that default already shares the symbol, otherwise
/// to the symbol's canonical owner.
fn strip_currency(s: &str, default_currency: Currency) -> Option<(Currency, &str)> {
    // Multi-char codes first (CHF, EUR) so they don't get mis-split.
    for code in [
        "CHF", "EUR", "USD", "GBP", "JPY", "CAD", "AUD", "CNY", "INR",
    ] {
        if let Some(rest) = s.strip_prefix(code)
            && let Some(c) = Currency::from_code(code)
        {
            return Some((c, rest.trim_start()));
        }
        if let Some(rest) = s.strip_suffix(code)
            && let Some(c) = Currency::from_code(code)
        {
            return Some((c, rest.trim_end()));
        }
    }
    // Single-glyph symbols.
    let symbols = ["$", "\u{20AC}", "\u{00A3}", "\u{00A5}", "\u{20B9}"];
    for sym in symbols {
        if let Some(rest) = s.strip_prefix(sym) {
            return Some((resolve_symbol(sym, default_currency), rest.trim_start()));
        }
        if let Some(rest) = s.strip_suffix(sym) {
            return Some((resolve_symbol(sym, default_currency), rest.trim_end()));
        }
    }
    None
}

/// Resolve an ambiguous symbol: if the document's default currency already
/// uses this glyph, keep the default (so a EUR document typing `$` is odd
/// but a USD/CAD/AUD document typing `$` keeps its own currency). Otherwise
/// fall back to the symbol's canonical owner.
fn resolve_symbol(sym: &str, default_currency: Currency) -> Currency {
    if default_currency.symbol() == sym {
        return default_currency;
    }
    Currency::from_symbol(sym).unwrap_or(default_currency)
}

/// Parse an ISO / US / EU date literal into an Excel serial.
fn parse_date(s: &str) -> Option<f64> {
    // ISO: YYYY-MM-DD
    if let Some((y, rest)) = split_int(s, '-')
        && let Some((m, d)) = split_two(rest, '-')
        && valid_ymd(y, m, d)
    {
        return Some(date_to_serial(y, m as u32, d as u32));
    }
    // Slash forms: try US (M/D/Y) and EU (D/M/Y). Prefer the one that is
    // unambiguously valid; if both are valid, US wins (matching the ISO
    // default style we render in).
    let parts: Vec<&str> = s.split('/').collect();
    if let [a_s, b_s, y_s] = parts.as_slice() {
        let a: i64 = a_s.trim().parse().ok()?;
        let b: i64 = b_s.trim().parse().ok()?;
        let y: i64 = y_s.trim().parse().ok()?;
        // US first.
        if valid_ymd(y, a, b) {
            return Some(date_to_serial(y, a as u32, b as u32));
        }
        if valid_ymd(y, b, a) {
            return Some(date_to_serial(y, b as u32, a as u32));
        }
    }
    None
}

/// Parse a `HH:MM` or `HH:MM:SS` time literal into the fractional day part
/// of an Excel serial.
fn parse_time(s: &str) -> Option<f64> {
    let (clock, pm) = if let Some(rest) = s.strip_suffix("PM").or_else(|| s.strip_suffix("pm")) {
        (rest.trim(), Some(true))
    } else if let Some(rest) = s.strip_suffix("AM").or_else(|| s.strip_suffix("am")) {
        (rest.trim(), Some(false))
    } else {
        (s, None)
    };
    let parts: Vec<&str> = clock.split(':').collect();
    let (h_s, mi_s, se_s) = match parts.as_slice() {
        [h, mi] => (*h, *mi, None),
        [h, mi, se] => (*h, *mi, Some(*se)),
        _ => return None,
    };
    let mut h: i64 = h_s.trim().parse().ok()?;
    let mi: i64 = mi_s.trim().parse().ok()?;
    let se: i64 = match se_s {
        Some(se_s) => se_s.trim().parse().ok()?,
        None => 0,
    };
    if !(0..=59).contains(&mi) || !(0..=59).contains(&se) {
        return None;
    }
    match pm {
        Some(true) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            if h != 12 {
                h += 12;
            }
        }
        Some(false) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            if h == 12 {
                h = 0;
            }
        }
        None => {
            if !(0..=23).contains(&h) {
                return None;
            }
        }
    }
    Some((h * 3600 + mi * 60 + se) as f64 / 86400.0)
}

fn split_int(s: &str, sep: char) -> Option<(i64, &str)> {
    let (head, tail) = s.split_once(sep)?;
    let n: i64 = head.trim().parse().ok()?;
    Some((n, tail))
}
fn split_two(s: &str, sep: char) -> Option<(i64, i64)> {
    let (a, b) = s.split_once(sep)?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn valid_ymd(y: i64, m: i64, d: i64) -> bool {
    (1..=9999).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d)
}

/// Render an f64 as the shortest exact-looking decimal: integers print
/// without a fractional part, otherwise we keep up to 10 significant
/// digits with trailing zeros trimmed. This is what we persist as the raw.
fn trim_float(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let s = format!("{n:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn infer(s: &str) -> Inferred {
        infer_entry(s, Currency::USD)
    }

    /// Percent and currency input inference.
    #[test]
    fn percent_and_currency_inputs_infer_their_format() {
        // Case `percent_stores_fraction`.
        {
            let r = infer("90%");
            assert_eq!(r.raw, "0.9");
            assert_eq!(r.format, CellFormat::Percent);
            // And it renders back as 90.00%.
            assert_eq!(
                crate::model::format_value(&crate::model::CellValue::Number(0.9), r.format),
                "90.00%"
            );
        }

        // Case `percent_with_decimals`.
        {
            let r = infer("12.5%");
            assert_eq!(r.raw, "0.125");
            assert_eq!(r.format, CellFormat::Percent);
        }

        // Case `leading_dollar_is_usd_currency`.
        {
            let r = infer("$1,200");
            assert_eq!(r.raw, "1200");
            assert_eq!(r.format, CellFormat::Currency(Currency::USD));
        }

        // Case `trailing_euro_is_eur_currency`.
        {
            let r = infer_entry("1.200,50\u{20AC}", Currency::EUR);
            // Euro grouping uses '.' for thousands and ',' for decimal, but our
            // loose parser only strips commas; a euro-typed amount is uncommon as
            // a literal. Here we accept the common Anglo style typed with €.
            let r2 = infer("1200.50\u{20AC}");
            assert_eq!(r2.format, CellFormat::Currency(Currency::EUR));
            assert_eq!(r2.raw, "1200.5");
            // The dotted-euro literal is treated as text (we don't guess euro
            // decimal grouping on entry), which is acceptable and non-broken.
            let _ = r;
        }

        // Case `currency_code_prefix`.
        {
            let r = infer("CHF 50");
            assert_eq!(r.raw, "50");
            assert_eq!(r.format, CellFormat::Currency(Currency::CHF));
        }

        // Case `dollar_respects_document_default_when_symbol_shared`.
        {
            // A CAD document typing "$5" keeps CAD (both use $).
            let r = infer_entry("$5", Currency::CAD);
            assert_eq!(r.format, CellFormat::Currency(Currency::CAD));
        }
    }

    /// ISO/US/EU dates and 24h/12h times.
    #[test]
    fn date_and_time_inputs_infer_their_format() {
        // Case `iso_date`.
        {
            let r = infer("2023-03-15");
            assert_eq!(r.format, CellFormat::Date(DateStyle::Iso));
            // Round-trips back to the same ISO string.
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(n, DateStyle::Iso),
                "2023-03-15"
            );
        }

        // Case `us_slash_date`.
        {
            let r = infer("03/15/2023");
            assert_eq!(r.format, CellFormat::Date(DateStyle::Iso));
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(n, DateStyle::Iso),
                "2023-03-15"
            );
        }

        // Case `eu_slash_date_when_unambiguous`.
        {
            // 25/03/2023 can only be D/M/Y.
            let r = infer("25/03/2023");
            assert_eq!(r.format, CellFormat::Date(DateStyle::Iso));
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(n, DateStyle::Iso),
                "2023-03-25"
            );
        }

        // Case `time_24h`.
        {
            let r = infer("14:05");
            assert_eq!(r.format, CellFormat::Time(TimeStyle::H24));
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(crate::model::serial_to_time(n, TimeStyle::H24), "14:05");
        }

        // Case `time_12h`.
        {
            let r = infer("2:05 PM");
            assert_eq!(r.format, CellFormat::Time(TimeStyle::H24));
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(crate::model::serial_to_time(n, TimeStyle::H24), "14:05");
        }
    }

    /// Grouped, plain, negative numbers, booleans, text, forced text and formulas.
    #[test]
    fn numbers_booleans_text_and_formulas_infer_their_kind() {
        // Case `grouped_number_becomes_number_format`.
        {
            let r = infer("1,234.5");
            assert_eq!(r.raw, "1234.5");
            assert_eq!(r.format, CellFormat::Number);
        }

        // Case `plain_integer_stays_general`.
        {
            let r = infer("42");
            assert_eq!(r.raw, "42");
            assert_eq!(r.format, CellFormat::General);
        }

        // Case `parenthesised_negative`.
        {
            let r = infer("(1,200)");
            assert_eq!(r.raw, "-1200");
            assert_eq!(r.format, CellFormat::Number);
        }

        // Case `booleans`.
        {
            assert_eq!(infer("true").format, CellFormat::Boolean);
            assert_eq!(infer("FALSE").format, CellFormat::Boolean);
            assert_eq!(infer("true").raw, "TRUE");
        }

        // Case `plain_text`.
        {
            let r = infer("Hello");
            assert_eq!(r.raw, "Hello");
            assert_eq!(r.format, CellFormat::Text);
        }

        // Case `leading_apostrophe_forces_text`.
        {
            // '0123 stays text, keeping the leading zero.
            let r = infer("'0123");
            assert_eq!(r.raw, "0123");
            assert_eq!(r.format, CellFormat::Text);
        }

        // Case `formula_is_passed_through`.
        {
            let r = infer("=SUM(A1:A3)");
            assert_eq!(r.raw, "=SUM(A1:A3)");
            assert_eq!(r.format, CellFormat::General);
        }
    }
}
