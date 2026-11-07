//! What a provider's answer costs.
//!
//! A person's agents plan with a model whose provider bills per token, and a
//! keeper bounds what an agent may spend in a day. Bounding it needs a price
//! for every call, so the trusted gateway prices each answer here, as list
//! prices in millionths of the currency per million tokens: an exact integer,
//! never a rounded estimate. The same figure is what the gateway's own log
//! records per call.
//!
//! The table is keyed by the model id its provider lists, most specific row
//! first, the way [`agent_provider_config::capabilities`] reads families. A
//! model no row names costs nothing here: an on-device model has no bill, and
//! an endpoint serving an open-weight model sets its own price, which Tilo
//! does not know and so never invents. Prices are the providers' published
//! list prices; a prompt the provider read from its cache is charged what that
//! provider charges for it.

use agent_provider_config::protocol::Usage;

/// One million, the base both a price and a token count are read against.
const MILLION: u128 = 1_000_000;

/// What one model's tokens cost, in millionths of the currency per million
/// tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Price {
    /// Per million input tokens that missed the provider's prompt cache.
    pub input: u64,
    /// Per million input tokens read from the provider's prompt cache.
    pub cached_input: u64,
    /// Per million input tokens written to the provider's prompt cache.
    pub cache_write: u64,
    /// Per million output tokens, reasoning included.
    pub output: u64,
}

impl Price {
    const fn new(input: u64, cached_input: u64, cache_write: u64, output: u64) -> Self {
        Self {
            input,
            cached_input,
            cache_write,
            output,
        }
    }

    /// What `usage` costs at this price, in millionths.
    ///
    /// [`Usage::input_tokens`] counts every input token, the cached and the
    /// written ones among them, so those are charged at their own price and
    /// the rest at the plain input price.
    #[must_use]
    pub fn of(&self, usage: &Usage) -> u64 {
        let cached = usage
            .cached_input_tokens
            .saturating_add(usage.cache_write_tokens)
            .min(usage.input_tokens);
        let priced = usage.input_tokens.saturating_sub(cached);
        let charged = |tokens: u64, millionths: u64| {
            u128::from(tokens)
                .saturating_mul(u128::from(millionths))
                .checked_div(MILLION)
                .unwrap_or_default()
        };
        let total = charged(priced, self.input)
            .saturating_add(charged(
                usage.cached_input_tokens.min(cached),
                self.cached_input,
            ))
            .saturating_add(charged(
                cached.saturating_sub(usage.cached_input_tokens),
                self.cache_write,
            ))
            .saturating_add(charged(usage.output_tokens, self.output));
        u64::try_from(total).unwrap_or(u64::MAX)
    }
}

/// One model family: the text its provider's model ids carry, and its price.
struct Row {
    pattern: &'static str,
    price: Price,
}

/// Known model families, most specific first: the first row whose pattern the
/// id contains decides. Prices are list prices in millionths of the currency
/// per million tokens: input, cache read, cache write, output.
const PRICES: &[Row] = &[
    // Anthropic: cache reads cost a tenth of the input, cache writes a
    // quarter more.
    Row {
        pattern: "claude-opus",
        price: Price::new(15_000_000, 1_500_000, 18_750_000, 75_000_000),
    },
    Row {
        pattern: "claude-sonnet",
        price: Price::new(3_000_000, 300_000, 3_750_000, 15_000_000),
    },
    Row {
        pattern: "claude-haiku",
        price: Price::new(1_000_000, 100_000, 1_250_000, 5_000_000),
    },
    Row {
        pattern: "claude",
        price: Price::new(3_000_000, 300_000, 3_750_000, 15_000_000),
    },
    // OpenAI: cache reads cost half the input, and there is no separate
    // write price.
    Row {
        pattern: "gpt-4o-mini",
        price: Price::new(150_000, 75_000, 150_000, 600_000),
    },
    Row {
        pattern: "gpt-4o",
        price: Price::new(2_500_000, 1_250_000, 2_500_000, 10_000_000),
    },
    Row {
        pattern: "gpt-4.1-mini",
        price: Price::new(400_000, 100_000, 400_000, 1_600_000),
    },
    Row {
        pattern: "gpt-4.1",
        price: Price::new(2_000_000, 500_000, 2_000_000, 8_000_000),
    },
    Row {
        pattern: "gpt-5",
        price: Price::new(1_250_000, 125_000, 1_250_000, 10_000_000),
    },
    Row {
        pattern: "o4-mini",
        price: Price::new(1_100_000, 275_000, 1_100_000, 4_400_000),
    },
    Row {
        pattern: "o3",
        price: Price::new(2_000_000, 500_000, 2_000_000, 8_000_000),
    },
    // Google.
    Row {
        pattern: "gemini-2.5-pro",
        price: Price::new(1_250_000, 312_500, 1_250_000, 10_000_000),
    },
    Row {
        pattern: "gemini-2.5-flash",
        price: Price::new(300_000, 75_000, 300_000, 2_500_000),
    },
    Row {
        pattern: "gemini",
        price: Price::new(100_000, 25_000, 100_000, 400_000),
    },
    // xAI.
    Row {
        pattern: "grok",
        price: Price::new(3_000_000, 750_000, 3_000_000, 15_000_000),
    },
    // DeepSeek: cache reads cost a tenth of the input.
    Row {
        pattern: "deepseek-reasoner",
        price: Price::new(550_000, 55_000, 550_000, 2_190_000),
    },
    Row {
        pattern: "deepseek",
        price: Price::new(270_000, 27_000, 270_000, 1_100_000),
    },
    // Mistral.
    Row {
        pattern: "mistral-large",
        price: Price::new(2_000_000, 500_000, 2_000_000, 6_000_000),
    },
    Row {
        pattern: "codestral",
        price: Price::new(300_000, 75_000, 300_000, 900_000),
    },
];

/// The price of model `id`, `None` for a model no row names.
#[must_use]
pub fn price_of(id: &str) -> Option<Price> {
    let id = id.to_ascii_lowercase();
    PRICES
        .iter()
        .find(|row| id.contains(row.pattern))
        .map(|row| row.price)
}

/// What one answer of model `id` costs, in millionths of the currency: zero
/// for a model no price is known for.
#[must_use]
pub fn cost_micros(id: &str, usage: &Usage) -> u64 {
    price_of(id).map_or(0, |price| price.of(usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Usage with the given counts.
    fn usage(input: u64, cached: u64, written: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input.saturating_add(output),
            cached_input_tokens: cached,
            cache_write_tokens: written,
            reasoning_tokens: 0,
        }
    }

    #[test]
    fn a_million_tokens_costs_the_rows_price() {
        let price = price_of("claude-sonnet-4-5").expect("a priced model");
        assert_eq!(price.of(&usage(1_000_000, 0, 0, 0)), 3_000_000);
        assert_eq!(price.of(&usage(0, 0, 0, 1_000_000)), 15_000_000);
    }

    #[test]
    fn the_most_specific_row_names_the_price() {
        let opus = cost_micros("claude-opus-4-1", &usage(1_000_000, 0, 0, 0));
        let sonnet = cost_micros("claude-sonnet-4-5", &usage(1_000_000, 0, 0, 0));
        let haiku = cost_micros("claude-haiku-4-5", &usage(1_000_000, 0, 0, 0));
        assert_eq!((opus, sonnet, haiku), (15_000_000, 3_000_000, 1_000_000));
        assert_eq!(
            cost_micros("gpt-4o-mini", &usage(1_000_000, 0, 0, 0)),
            150_000
        );
        assert_eq!(cost_micros("gpt-4o", &usage(1_000_000, 0, 0, 0)), 2_500_000);
    }

    #[test]
    fn a_cached_prompt_is_charged_what_its_provider_charges() {
        // A provider counts the tokens it read from its cache in the input;
        // they cost a tenth of it at Anthropic, half at OpenAI.
        let cached = usage(1_000_000, 1_000_000, 0, 0);
        assert_eq!(cost_micros("claude-sonnet-4-5", &cached), 300_000);
        assert_eq!(cost_micros("gpt-4o", &cached), 1_250_000);
        // Written to the cache: a quarter more than the input.
        let written = usage(1_000_000, 0, 1_000_000, 0);
        assert_eq!(cost_micros("claude-sonnet-4-5", &written), 3_750_000);
        // Half cached, half not: each part at its own price.
        let half = usage(1_000_000, 500_000, 0, 0);
        assert_eq!(cost_micros("claude-sonnet-4-5", &half), 1_650_000);
    }

    #[test]
    fn a_model_with_no_price_is_not_charged() {
        assert_eq!(
            cost_micros("llama-3.3-70b", &usage(1_000_000, 0, 0, 1_000)),
            0
        );
        assert_eq!(cost_micros("my-local-model", &usage(10, 0, 0, 10)), 0);
        assert!(price_of("llama-3.3-70b").is_none());
    }

    #[test]
    fn a_prompt_never_costs_more_than_its_tokens_are_counted() {
        // More cached tokens than the candidate says were used is a broken
        // count, not a windfall: only the counted input is charged.
        let broken = usage(10, 1_000, 1_000, 0);
        assert_eq!(cost_micros("claude-sonnet-4-5", &broken), 3);
        // A reply with no usage counted costs nothing.
        assert_eq!(cost_micros("claude-sonnet-4-5", &Usage::default()), 0);
    }
}
