//! Money typed into search: "10 usd in eur", "€25 to $", "100 franken".
//! The amount converts at the last reference rates the host fetched
//! ([`Rates`]), never on the keystroke.

use std::collections::BTreeMap;

/// Reference exchange rates the host fetched, and when.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rates {
    /// ISO 4217 code of the currency every rate is against.
    pub base: String,
    /// ISO 4217 code → units of it per one `base`.
    pub rates: BTreeMap<String, f64>,
    /// When they were fetched, in Unix seconds.
    pub fetched_at: u64,
}

impl Rates {
    /// `amount` of currency `from` in currency `to`, if both are known.
    #[must_use]
    pub fn convert(&self, amount: f64, from: &str, to: &str) -> Option<f64> {
        let per_base = |code: &str| {
            if code == self.base {
                Some(1.0)
            } else {
                self.rates.get(code).copied().filter(|rate| *rate > 0.0)
            }
        };
        let value = amount / per_base(from)? * per_base(to)?;
        value.is_finite().then_some(value)
    }
}

/// The ISO 4217 code a typed currency name, symbol or code stands for.
fn code(typed: &str) -> Option<String> {
    let typed = typed.trim().to_lowercase();
    let known = match typed.as_str() {
        "€" | "euro" | "euros" => "EUR",
        "$" | "us$" | "dollar" | "dollars" | "usd$" => "USD",
        "£" | "pound" | "pounds" | "pfund" => "GBP",
        "¥" | "yen" => "JPY",
        "fr" | "franken" | "franc" | "francs" | "chf" => "CHF",
        "kr" | "kronor" | "kronen" => "SEK",
        "zł" | "zloty" => "PLN",
        "₹" | "rupee" | "rupees" => "INR",
        "₩" | "won" => "KRW",
        "yuan" | "renminbi" | "rmb" => "CNY",
        _ => "",
    };
    if !known.is_empty() {
        return Some(known.to_owned());
    }
    (typed.len() == 3 && typed.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| typed.to_ascii_uppercase())
}

/// The amount and currency of `text` ("10 usd", "€25", "25€").
fn amount(text: &str) -> Option<(f64, String)> {
    let text = text.trim();
    let split = text
        .find(|c: char| c.is_ascii_digit())
        .filter(|at| *at > 0)
        .map(|at| text.split_at(at));
    // A symbol or code before the number ("€25", "usd 10").
    if let Some((before, number)) = split
        && let Ok(value) = number.trim().replace(',', ".").parse::<f64>()
    {
        return Some((value, code(before)?));
    }
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(end);
    let value: f64 = number.replace(',', ".").parse().ok()?;
    Some((value, code(unit)?))
}

/// What `typed` asks to convert: the amount, its currency and the target.
/// Without a target, euros go to dollars and everything else to euros.
pub fn request(typed: &str) -> Option<(f64, String, String)> {
    let text = typed.trim().to_lowercase();
    if !text.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let parts = [
        " in ", " to ", " into ", " as ", " -> ", " = ", " nach ", " zu ", " als ",
    ]
    .iter()
    .find_map(|word| text.split_once(word));
    let (from_text, to) = match parts {
        Some((from, to)) => (from, Some(code(to)?)),
        None => (text.as_str(), None),
    };
    let (value, from) = amount(from_text)?;
    let to = to.unwrap_or_else(|| if from == "EUR" { "USD" } else { "EUR" }.to_owned());
    (from != to).then_some((value, from, to))
}

/// `typed` converted at `rates`: the amount as typed and as answered
/// ("10.00 USD", "9.25 EUR").
pub fn convert(typed: &str, rates: &Rates) -> Option<(String, String)> {
    let (value, from, to) = request(typed)?;
    let answer = rates.convert(value, &from, &to)?;
    Some((format!("{value:.2} {from}"), format!("{answer:.2} {to}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rates() -> Rates {
        Rates {
            base: "EUR".into(),
            rates: [("USD".to_owned(), 1.25), ("GBP".to_owned(), 0.8)]
                .into_iter()
                .collect(),
            fetched_at: 0,
        }
    }

    #[test]
    fn money_converts_at_the_kept_rates() {
        let rates = rates();
        assert_eq!(
            convert("10 usd in eur", &rates),
            Some(("10.00 USD".into(), "8.00 EUR".into()))
        );
        assert_eq!(
            convert("€25 to $", &rates),
            Some(("25.00 EUR".into(), "31.25 USD".into()))
        );
        assert_eq!(
            convert("100 eur", &rates),
            Some(("100.00 EUR".into(), "125.00 USD".into())),
            "euros go to dollars when nothing is asked"
        );
        assert_eq!(
            convert("2,4 pfund nach euro", &rates),
            Some(("2.40 GBP".into(), "3.00 EUR".into()))
        );
        assert_eq!(convert("10 km in mi", &rates), None);
        assert_eq!(convert("usd", &rates), None);
        assert_eq!(convert("10 xyz in eur", &rates), None, "an unknown code");
        assert_eq!(convert("10 eur in eur", &rates), None);
    }
}
