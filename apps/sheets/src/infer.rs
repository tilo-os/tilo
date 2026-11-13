//! Smart type inference on entry: `90%` becomes 0.9 shown as a percent, `$1,200` becomes 1200 shown
//! as money, `2023-03-15` becomes a date. [`infer_entry`] answers the canonical raw to store and
//! the format to show it in; formulas and quoted text pass through untouched.

use crate::model::{CellFormat, date_to_serial};
use tilo_locale::{Currency, DateStyle};
use ui::clock::TimeStyle;

#[derive(Clone, Debug, PartialEq)]
pub struct Inferred {
    /// The canonical raw to store.
    pub raw: String,
    pub format: CellFormat,
}

/// The raw and format a literal entry stands for; `default_currency` answers for bare money
/// symbols.
pub fn infer_entry(input: &str, default_currency: Currency) -> Inferred {
    let trimmed = input.trim();

    // Formulas and empty cells are never reinterpreted.
    if trimmed.is_empty() || trimmed.starts_with('=') {
        return Inferred {
            raw: input.to_string(),
            format: CellFormat::General,
        };
    }

    // A leading apostrophe forces text; the quote is dropped.
    if trimmed.len() >= 2 && trimmed.starts_with('\'') {
        return Inferred {
            raw: trimmed.get(1..).unwrap_or("").to_string(),
            format: CellFormat::Text,
        };
    }

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

    // A trailing `%` is a percentage.
    if let Some(body) = trimmed.strip_suffix('%')
        && let Some(n) = parse_number_loose(body.trim())
    {
        // The fraction is stored; the percent format multiplies by 100.
        return Inferred {
            raw: trim_float(n / 100.0),
            format: CellFormat::Percent,
        };
    }

    if let Some((cur, amount)) = strip_currency(trimmed, default_currency)
        && let Some(n) = parse_number_loose(amount)
    {
        return Inferred {
            raw: trim_float(n),
            format: CellFormat::Currency(cur),
        };
    }

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

    // Grouped digits store the bare number with the Number format, so the grouping shows.
    if has_grouping(trimmed)
        && let Some(n) = parse_number_loose(trimmed)
    {
        return Inferred {
            raw: trim_float(n),
            format: CellFormat::Number,
        };
    }

    // A plain number keeps its raw and stays General, so integers stay clean.
    if let Some(n) = parse_number_loose(trimmed) {
        return Inferred {
            raw: trim_float(n),
            format: CellFormat::General,
        };
    }

    Inferred {
        raw: input.to_string(),
        format: CellFormat::Text,
    }
}

/// A number with surrounding space, thousands separators or a parenthesised negative.
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
    // Reject `1.2.3`: one sign, digits, one dot and an optional exponent only.
    let n: f64 = cleaned.trim().parse().ok()?;
    Some(if neg { -n } else { n })
}

/// Whether the string groups thousands, so the Number format keeps the look.
fn has_grouping(s: &str) -> bool {
    // A comma between digits.
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

/// A leading or trailing currency marker, with the currency it names and the text left.
fn strip_currency(s: &str, default_currency: Currency) -> Option<(Currency, &str)> {
    // Multi-character codes first, so they are not split.
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

/// An ambiguous symbol in the document's own currency stays that currency, else it takes the
/// symbol's canonical owner.
fn resolve_symbol(sym: &str, default_currency: Currency) -> Currency {
    if default_currency.symbol() == sym {
        return default_currency;
    }
    Currency::from_symbol(sym).unwrap_or(default_currency)
}

/// An ISO, US or EU date literal as an Excel serial.
fn parse_date(s: &str) -> Option<f64> {
    if let Some((y, rest)) = split_int(s, '-')
        && let Some((m, d)) = split_two(rest, '-')
        && valid_ymd(y, m, d)
    {
        return Some(date_to_serial(y, m as u32, d as u32));
    }
    // Slash forms: the reading only one way wins, and M/D/Y when both are valid.
    let parts: Vec<&str> = s.split('/').collect();
    if let [a_s, b_s, y_s] = parts.as_slice() {
        let a: i64 = a_s.trim().parse().ok()?;
        let b: i64 = b_s.trim().parse().ok()?;
        let y: i64 = y_s.trim().parse().ok()?;
        if valid_ymd(y, a, b) {
            return Some(date_to_serial(y, a as u32, b as u32));
        }
        if valid_ymd(y, b, a) {
            return Some(date_to_serial(y, b as u32, a as u32));
        }
    }
    None
}

/// A `HH:MM` or `HH:MM:SS` literal as the fraction of a day.
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

/// An f64 as the shortest exact-looking decimal: up to ten significant digits, trailing zeros
/// trimmed.
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

    #[test]
    fn percent_and_currency_inputs_infer_their_format() {
        {
            let r = infer("90%");
            assert_eq!(r.raw, "0.9");
            assert_eq!(r.format, CellFormat::Percent);
            assert_eq!(
                crate::model::format_value(&crate::model::CellValue::Number(0.9), r.format),
                "90.00%"
            );
        }

        {
            let r = infer("12.5%");
            assert_eq!(r.raw, "0.125");
            assert_eq!(r.format, CellFormat::Percent);
        }

        {
            let r = infer("$1,200");
            assert_eq!(r.raw, "1200");
            assert_eq!(r.format, CellFormat::Currency(Currency::USD));
        }

        {
            let r = infer_entry("1.200,50\u{20AC}", Currency::EUR);
            let r2 = infer("1200.50\u{20AC}");
            assert_eq!(r2.format, CellFormat::Currency(Currency::EUR));
            assert_eq!(r2.raw, "1200.5");
            let _ = r;
        }

        {
            let r = infer("CHF 50");
            assert_eq!(r.raw, "50");
            assert_eq!(r.format, CellFormat::Currency(Currency::CHF));
        }

        {
            let r = infer_entry("$5", Currency::CAD);
            assert_eq!(r.format, CellFormat::Currency(Currency::CAD));
        }
    }

    #[test]
    fn date_and_time_inputs_infer_their_format() {
        {
            let r = infer("2023-03-15");
            assert_eq!(r.format, CellFormat::Date(DateStyle::Iso));
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(n, DateStyle::Iso),
                "2023-03-15"
            );
        }

        {
            let r = infer("03/15/2023");
            assert_eq!(r.format, CellFormat::Date(DateStyle::Iso));
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(n, DateStyle::Iso),
                "2023-03-15"
            );
        }

        {
            let r = infer("25/03/2023");
            assert_eq!(r.format, CellFormat::Date(DateStyle::Iso));
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(
                crate::model::serial_to_date(n, DateStyle::Iso),
                "2023-03-25"
            );
        }

        {
            let r = infer("14:05");
            assert_eq!(r.format, CellFormat::Time(TimeStyle::H24));
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(crate::model::serial_to_time(n, TimeStyle::H24), "14:05");
        }

        {
            let r = infer("2:05 PM");
            assert_eq!(r.format, CellFormat::Time(TimeStyle::H24));
            let n: f64 = r.raw.parse().unwrap();
            assert_eq!(crate::model::serial_to_time(n, TimeStyle::H24), "14:05");
        }
    }

    #[test]
    fn numbers_booleans_text_and_formulas_infer_their_kind() {
        {
            let r = infer("1,234.5");
            assert_eq!(r.raw, "1234.5");
            assert_eq!(r.format, CellFormat::Number);
        }

        {
            let r = infer("42");
            assert_eq!(r.raw, "42");
            assert_eq!(r.format, CellFormat::General);
        }

        {
            let r = infer("(1,200)");
            assert_eq!(r.raw, "-1200");
            assert_eq!(r.format, CellFormat::Number);
        }

        {
            assert_eq!(infer("true").format, CellFormat::Boolean);
            assert_eq!(infer("FALSE").format, CellFormat::Boolean);
            assert_eq!(infer("true").raw, "TRUE");
        }

        {
            let r = infer("Hello");
            assert_eq!(r.raw, "Hello");
            assert_eq!(r.format, CellFormat::Text);
        }

        {
            let r = infer("'0123");
            assert_eq!(r.raw, "0123");
            assert_eq!(r.format, CellFormat::Text);
        }

        {
            let r = infer("=SUM(A1:A3)");
            assert_eq!(r.raw, "=SUM(A1:A3)");
            assert_eq!(r.format, CellFormat::General);
        }
    }
}
