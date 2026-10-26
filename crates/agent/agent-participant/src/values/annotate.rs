//! Dates and amounts in what a document says, read by the host and written
//! beside them for the model: `14 October 2026 [2026-10-14]`, `03/04/1988
//! [1988-04-03]`, `€1.234,50 [1234.50]`.
//!
//! A model reading a PDF, a Word document, a scan's text or a mail turns
//! `03/04/1988` into a date itself, and guesses its order. The host reads
//! the order from the document where it can: a date elsewhere in it whose
//! day is past 12 (`14/08/2024`), dots (`14.10.2026`, written day first),
//! or a stated `DD/MM/YYYY`. Where nothing settles it, both readings are
//! given, and which is meant stays the model's to find out. Amounts are
//! annotated only where their separators could be misread.
//!
//! What the model copies from a document into a step may carry these
//! notes along: [`unannotated`] takes them out of the step's arguments
//! again, only right after a date or amount they were written for.

use std::sync::LazyLock;

use regex::Regex;

use super::dates::{self, Numeric};

/// Numbers joined by one separator: `03/04/1988`, `14.10.2026`.
static NUMERIC: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\b\d{1,2}[/.\-]\d{1,2}[/.\-]\d{4}\b").ok());

/// A month by name, day first or month first: `14 October 2026`, `14th of
/// Oct. 2026`, `October 14, 2026`.
static WRITTEN: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:\d{1,2}(?:st|nd|rd|th)?(?:\s+of)?\s+[a-z]{3,9}\.?,?\s+\d{4}|[a-z]{3,9}\.?\s+\d{1,2}(?:st|nd|rd|th)?,?\s+\d{4})\b",
    )
    .ok()
});

/// An amount with its currency before or after it.
static AMOUNT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?:[€$£¥]\s?|\b(?:EUR|USD|GBP|CHF|JPY|CAD|AUD|SEK|NOK|DKK)\s?)(\d{1,3}(?:[.,]\d{3})+(?:[.,]\d{1,2})?|\d+,\d{1,2})\b|\b(\d{1,3}(?:[.,]\d{3})+(?:[.,]\d{1,2})?|\d+,\d{1,2})\s?(?:€|EUR\b|USD\b|GBP\b|CHF\b|JPY\b|CAD\b|AUD\b|SEK\b|NOK\b|DKK\b)",
    )
    .ok()
});

/// A stated order: `DD/MM/YYYY`, `MM-DD-YY`.
static STATED: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)\b(dd?|mm?)[/.\-](dd?|mm?)[/.\-]yy(?:yy)?\b").ok());

/// Which way a text writes its dates of numbers, from those only one
/// order reads and from what it states.
#[derive(Clone, Copy, Debug, Default)]
struct Order {
    day_first: usize,
    month_first: usize,
}

impl Order {
    fn learn(&mut self, text: &str) {
        if let Some(numeric) = NUMERIC.as_ref() {
            for found in numeric.find_iter(text) {
                let written = found.as_str();
                let mut parts = written.split(['/', '.', '-']);
                let first: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
                let second: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
                if written.contains('.') || (first > 12 && second <= 12) {
                    self.day_first = self.day_first.saturating_add(1);
                } else if second > 12 && first <= 12 {
                    self.month_first = self.month_first.saturating_add(1);
                }
            }
        }
        if let Some(stated) = STATED.as_ref() {
            for found in stated.captures_iter(text) {
                let first = found.get(1).map_or("", |part| part.as_str());
                if first.eq_ignore_ascii_case("d") || first.eq_ignore_ascii_case("dd") {
                    self.day_first = self.day_first.saturating_add(1);
                } else {
                    self.month_first = self.month_first.saturating_add(1);
                }
            }
        }
    }

    /// Day first (`Some(true)`) or month first, when the text says only
    /// one.
    const fn settled(self) -> Option<bool> {
        match (self.day_first > 0, self.month_first > 0) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        }
    }
}

/// A date or amount found in a text: where it is and what the host reads
/// it as.
struct Mention {
    start: usize,
    end: usize,
    note: String,
}

/// Every date and amount of `text` the host reads, in order, none
/// overlapping; numbers read either way follow `order`.
fn mentions(text: &str, order: Option<bool>) -> Vec<Mention> {
    let mut found = Vec::new();
    if let Some(numeric) = NUMERIC.as_ref() {
        for hit in numeric.find_iter(text) {
            let Some((reading, separator)) = dates::numeric_date(hit.as_str()) else {
                continue;
            };
            let note = match (
                reading,
                order.or_else(|| (separator == '.').then_some(true)),
            ) {
                (Numeric::Settled(day), _) => dates::iso(day),
                (reading, Some(day_first)) => dates::iso(reading.read(day_first)),
                (reading, None) => format!(
                    "{} if day first, {} if month first",
                    dates::iso(reading.read(true)),
                    dates::iso(reading.read(false))
                ),
            };
            found.push(Mention {
                start: hit.start(),
                end: hit.end(),
                note,
            });
        }
    }
    if let Some(written) = WRITTEN.as_ref() {
        for hit in written.find_iter(text) {
            if let Some(day) = dates::written_date(&hit.as_str().replace('.', " ")) {
                found.push(Mention {
                    start: hit.start(),
                    end: hit.end(),
                    note: dates::iso(day),
                });
            }
        }
    }
    if let Some(amount) = AMOUNT.as_ref() {
        for hit in amount.captures_iter(text) {
            let (Some(whole), Some(number)) = (hit.get(0), hit.get(1).or_else(|| hit.get(2)))
            else {
                continue;
            };
            if let Some(note) = plain_amount(number.as_str()) {
                found.push(Mention {
                    start: whole.start(),
                    end: whole.end(),
                    note,
                });
            }
        }
    }
    found.sort_by_key(|mention| (mention.start, std::cmp::Reverse(mention.end)));
    let mut kept: Vec<Mention> = Vec::with_capacity(found.len());
    for mention in found {
        if kept.last().is_none_or(|last| mention.start >= last.end) {
            kept.push(mention);
        }
    }
    kept
}

/// The amount `written` (`1.234,50`, `3,412.50`, `289,00`) as a plain
/// number, when its separators say which is the decimal point; `None` when
/// they do not, or it is plain already.
fn plain_amount(written: &str) -> Option<String> {
    let last = written.rfind(['.', ','])?;
    let decimals = written.get(last.saturating_add(1)..)?;
    let whole = written.get(..last)?;
    let (whole, decimals) = if decimals.len() == 3 {
        // `1,234` and `1.234.567` group thousands; a lone `1.234` could
        // be a decimal.
        if written.matches(['.', ',']).count() == 1 && written.contains('.') {
            return None;
        }
        (written, "")
    } else {
        (whole, decimals)
    };
    let digits: String = whole.chars().filter(char::is_ascii_digit).collect();
    let plain = if decimals.is_empty() {
        digits
    } else {
        format!("{digits}.{decimals}")
    };
    (plain != written).then_some(plain)
}

/// `text` with each date and amount the host reads noted after it, numbers
/// read either way as `order` says.
fn noted(text: &str, order: Option<bool>) -> String {
    let found = mentions(text, order);
    if found.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len().saturating_add(found.len().saturating_mul(16)));
    let mut at = 0;
    for mention in found {
        out.push_str(text.get(at..mention.end).unwrap_or_default());
        // A text noted already keeps its note.
        if !text
            .get(mention.end..)
            .unwrap_or_default()
            .starts_with(" [")
        {
            out.push_str(" [");
            out.push_str(&mention.note);
            out.push(']');
        }
        at = mention.end;
    }
    out.push_str(text.get(at..).unwrap_or_default());
    out
}

/// `text` with its dates and amounts noted, the order of its dates read
/// from it alone.
pub(crate) fn annotate_text(text: &str) -> String {
    let mut order = Order::default();
    order.learn(text);
    noted(text, order.settled())
}

/// `value` with the dates and amounts of every text in it noted, the order
/// of its dates read from all of them: one document.
pub(crate) fn annotate_json(value: &serde_json::Value) -> serde_json::Value {
    fn learn(value: &serde_json::Value, order: &mut Order) {
        match value {
            serde_json::Value::String(text) if !text.starts_with("data:") => order.learn(text),
            serde_json::Value::Array(items) => items.iter().for_each(|item| learn(item, order)),
            serde_json::Value::Object(fields) => {
                fields.values().for_each(|field| learn(field, order));
            }
            _ => {}
        }
    }
    fn note(value: &serde_json::Value, order: Option<bool>) -> serde_json::Value {
        match value {
            serde_json::Value::String(text) if !text.starts_with("data:") => {
                serde_json::Value::String(noted(text, order))
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(|item| note(item, order)).collect())
            }
            serde_json::Value::Object(fields) => serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(key, field)| (key.clone(), note(field, order)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    let mut order = Order::default();
    learn(value, &mut order);
    note(value, order.settled())
}

/// The most characters a note takes, its brackets left out.
const MOST_NOTE_CHARS: usize = 64;

/// `text` without the notes [`annotate_text`] writes after a date or an
/// amount: ` [` and a note starting with a digit, right after one. `None`
/// when it has none.
pub(crate) fn unannotated(text: &str) -> Option<String> {
    if !text.contains(" [") {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut changed = false;
    for mention in mentions(text, Some(true)) {
        let after = text.get(mention.end..).unwrap_or_default();
        let Some(inner) = after.strip_prefix(" [") else {
            continue;
        };
        let Some(close) = inner.find(']') else {
            continue;
        };
        let note = inner.get(..close).unwrap_or_default();
        if note.chars().count() > MOST_NOTE_CHARS
            || note.contains('\n')
            || !note.starts_with(|c: char| c.is_ascii_digit())
        {
            continue;
        }
        out.push_str(text.get(at..mention.end).unwrap_or_default());
        at = mention.end.saturating_add(close).saturating_add(3);
        changed = true;
    }
    if !changed {
        return None;
    }
    out.push_str(text.get(at..).unwrap_or_default());
    Some(out)
}
