//! Answers search gets from the network, and their wire format.
//!
//! The compositor never opens a socket. When search needs something only
//! the network knows (today: exchange rates), it runs the `tilo-net-answers`
//! helper, which confines itself before it fetches anything (it may read
//! the trust bundle and the resolver's files and connect to HTTPS, nothing
//! else), and prints one [`Answer`] as JSON on stdout. The shell keeps the
//! last answer, with when it was fetched, and answers from it offline.
//!
//! Each kind of answer is a [`Source`]; adding one is a variant, its URL and
//! its parser.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What the helper can fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Euro reference exchange rates, published each working day by the
    /// European Central Bank.
    Rates,
}

impl Source {
    /// The name the helper is asked for it by.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rates => "rates",
        }
    }

    /// The source by its name.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        (name == Self::Rates.name()).then_some(Self::Rates)
    }

    /// Where it is published.
    #[must_use]
    pub const fn url(self) -> &'static str {
        match self {
            Self::Rates => "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml",
        }
    }

    /// The answer in a fetched `body`.
    ///
    /// # Errors
    ///
    /// The body does not hold what the source publishes.
    pub fn parse(self, body: &str) -> Result<Answer, String> {
        match self {
            Self::Rates => parse_ecb_rates(body).map(Answer::Rates),
        }
    }
}

/// One fetched answer, as the helper prints it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Answer {
    Rates(Rates),
}

/// Exchange rates: how many of each currency one `base` buys, on `date`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    /// ISO 4217 code of the currency every rate is against.
    pub base: String,
    /// The day the rates are for, `YYYY-MM-DD`, as published.
    pub date: String,
    /// ISO 4217 code → units of it per one `base`.
    pub rates: BTreeMap<String, f64>,
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

/// The value of `name='…'` (or `name="…"`) in `element`.
fn attribute<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let at = element.find(&format!("{name}="))?;
    let rest = element.get(at.checked_add(name.len())?.checked_add(1)?..)?;
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let value = rest.get(1..)?;
    value.get(..value.find(quote)?)
}

/// The ECB's daily reference rates: `<Cube time='…'>` holding one
/// `<Cube currency='USD' rate='1.0812'/>` per currency, against the euro.
fn parse_ecb_rates(body: &str) -> Result<Rates, String> {
    let mut date = None;
    let mut rates = BTreeMap::new();
    for element in body
        .split('<')
        .filter(|element| element.starts_with("Cube"))
    {
        if let Some(time) = attribute(element, "time") {
            date = Some(time.to_owned());
        }
        if let (Some(code), Some(rate)) =
            (attribute(element, "currency"), attribute(element, "rate"))
        {
            let code = code.trim().to_ascii_uppercase();
            let rate: f64 = rate
                .trim()
                .parse()
                .map_err(|_| format!("rate of {code} is not a number"))?;
            if code.len() == 3 && code.chars().all(|c| c.is_ascii_alphabetic()) && rate > 0.0 {
                rates.insert(code, rate);
            }
        }
    }
    let date = date.ok_or("no date in the reference rates")?;
    if rates.is_empty() {
        return Err("no rates in the reference rates".into());
    }
    Ok(Rates {
        base: "EUR".into(),
        date,
        rates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
	<gesmes:subject>Reference rates</gesmes:subject>
	<Cube>
		<Cube time='2026-10-16'>
			<Cube currency='USD' rate='1.0812'/>
			<Cube currency='JPY' rate='162.31'/>
			<Cube currency='GBP' rate='0.8402'/>
		</Cube>
	</Cube>
</gesmes:Envelope>"#;

    #[test]
    fn the_ecb_reference_rates_parse_and_convert_through_the_euro() {
        let Answer::Rates(rates) = Source::Rates.parse(ECB).unwrap();
        assert_eq!(rates.date, "2026-10-16");
        assert_eq!(rates.base, "EUR");
        assert_eq!(rates.rates.len(), 3);
        let usd = rates.convert(10.0, "EUR", "USD").unwrap();
        assert!((usd - 10.812).abs() < 1e-9);
        let eur = rates.convert(10.812, "USD", "EUR").unwrap();
        assert!((eur - 10.0).abs() < 1e-9);
        let gbp = rates.convert(1.0812, "USD", "GBP").unwrap();
        assert!((gbp - 0.8402).abs() < 1e-9);
        assert_eq!(rates.convert(1.0, "USD", "XYZ"), None);
        assert!(Source::Rates.parse("<html>maintenance</html>").is_err());
    }

    #[test]
    fn an_answer_round_trips_through_its_wire_form() {
        let answer = Source::Rates.parse(ECB).unwrap();
        let wire = serde_json::to_string(&answer).unwrap();
        assert!(wire.contains("\"source\":\"rates\""));
        assert_eq!(serde_json::from_str::<Answer>(&wire).unwrap(), answer);
        assert_eq!(Source::named("rates"), Some(Source::Rates));
        assert_eq!(Source::named("weather"), None);
    }
}
