//! World currencies, by ISO-4217 code: the symbol, the placement and the conventional number of
//! decimals an amount is written with. The `locale` section of a person's settings picks the one
//! amounts are shown in, and a spreadsheet cell's own format names one, so every formatter that
//! writes money reads them here rather than carrying its own table.

/// World currency, by ISO-4217 code. Each carries its own symbol,
/// placement, and conventional number of decimals — `$1,234.50` vs
/// `1.234,50 €` vs `¥1,235` (yen has no minor unit). The serde repr is the
/// bare variant name, which equals the ISO code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::Model)]
pub enum Currency {
    #[model(rename = "USD")]
    USD,
    #[model(rename = "EUR")]
    EUR,
    #[model(rename = "GBP")]
    GBP,
    #[model(rename = "JPY")]
    JPY,
    #[model(rename = "CHF")]
    CHF,
    #[model(rename = "CAD")]
    CAD,
    #[model(rename = "AUD")]
    AUD,
    #[model(rename = "CNY")]
    CNY,
    #[model(rename = "INR")]
    INR,
}

impl Currency {
    /// Display symbol (or ISO code where there's no common glyph).
    pub fn symbol(self) -> &'static str {
        match self {
            Currency::USD | Currency::CAD | Currency::AUD => "$",
            Currency::EUR => "\u{20AC}",
            Currency::GBP => "\u{00A3}",
            Currency::JPY | Currency::CNY => "\u{00A5}",
            Currency::CHF => "CHF",
            Currency::INR => "\u{20B9}",
        }
    }

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

    pub fn decimals(self) -> usize {
        match self {
            Currency::JPY => 0,
            _ => 2,
        }
    }

    pub fn symbol_leads(self) -> bool {
        !matches!(self, Currency::EUR | Currency::CHF)
    }

    /// Map a bare currency *symbol* (as typed) to a currency; an ambiguous
    /// one (`$`, `¥`) resolves to its most common owner.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_currency_reads_back_from_its_code() {
        for currency in Currency::ALL {
            assert_eq!(Currency::from_code(currency.code()), Some(currency));
        }
        assert_eq!(Currency::from_code("usd"), Some(Currency::USD));
        assert_eq!(Currency::from_code("XXX"), None);
    }
}
