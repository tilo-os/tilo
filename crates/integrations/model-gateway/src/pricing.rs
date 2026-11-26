//! What a provider's answer costs.
//!
//! A person's agents plan with a model whose provider bills per token, and a
//! workspace bounds what an agent may spend in a day. Bounding it needs a price
//! for every call, so the trusted gateway prices each answer here, as list
//! prices in millionths of the currency per million tokens: an exact integer,
//! never a rounded estimate. The gateway's own log records the same figure
//! per call where the table lists the model itself, and says it is the
//! table's ([`listed_price_of`]).
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
    // Anthropic, the current models first: cache writes cost a quarter more
    // than the input, cache reads a tenth of it or what the row says.
    Row {
        pattern: "claude-fable-5-1",
        price: Price::new(10_000_000, 250_000, 12_500_000, 50_000_000),
    },
    Row {
        pattern: "claude-mythos-5-1",
        price: Price::new(10_000_000, 250_000, 12_500_000, 50_000_000),
    },
    Row {
        pattern: "claude-fable-5",
        price: Price::new(10_000_000, 1_000_000, 12_500_000, 50_000_000),
    },
    Row {
        pattern: "claude-mythos-5",
        price: Price::new(10_000_000, 1_000_000, 12_500_000, 50_000_000),
    },
    Row {
        pattern: "claude-opus-5-5",
        price: Price::new(4_000_000, 200_000, 5_000_000, 20_000_000),
    },
    Row {
        pattern: "claude-opus-5",
        price: Price::new(5_000_000, 500_000, 6_250_000, 25_000_000),
    },
    Row {
        pattern: "claude-opus-4-8",
        price: Price::new(5_000_000, 500_000, 6_250_000, 25_000_000),
    },
    Row {
        pattern: "claude-opus-4-7",
        price: Price::new(5_000_000, 500_000, 6_250_000, 25_000_000),
    },
    Row {
        pattern: "claude-opus-4-6",
        price: Price::new(5_000_000, 500_000, 6_250_000, 25_000_000),
    },
    Row {
        pattern: "claude-sonnet-5",
        price: Price::new(2_000_000, 200_000, 2_500_000, 10_000_000),
    },
    // Its price for a prompt of up to a hundred thousand tokens; a longer
    // one costs five times as much, which a price per token cannot say.
    Row {
        pattern: "claude-haiku-5",
        price: Price::new(100_000, 10_000, 125_000, 500_000),
    },
    // Anthropic's earlier models: cache reads cost a tenth of the input,
    // cache writes a quarter more.
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

/// The patterns that name a whole family and no model of it. Such a row
/// prices whatever newer model the family gains, at a guess: enough to
/// bound a day's spending by, and nothing to read as that model's price.
const FAMILIES: &[&str] = &[
    "claude-opus",
    "claude-sonnet",
    "claude-haiku",
    "claude",
    "gemini",
    "grok",
    "deepseek",
];

/// The price the table lists for model `id` itself: `None` for a model no
/// row names, and for one only a family's row would price.
#[must_use]
pub fn listed_price_of(id: &str) -> Option<Price> {
    let id = id.to_ascii_lowercase();
    PRICES
        .iter()
        .find(|row| id.contains(row.pattern))
        .filter(|row| !FAMILIES.contains(&row.pattern))
        .map(|row| row.price)
}

/// What one answer of model `id` costs, in millionths of the currency: zero
/// for a model no price is known for.
#[must_use]
pub fn cost_micros(id: &str, usage: &Usage) -> u64 {
    price_of(id).map_or(0, |price| price.of(usage))
}
