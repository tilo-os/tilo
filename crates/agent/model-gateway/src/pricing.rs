//! What a provider's answer costs.
//!
//! A person's agents plan with a model whose provider bills per token, and a
//! keeper bounds what an agent may spend in a day. Bounding it needs a price
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

/// How one answer's cost was determined against the price table.
///
/// Distinguishes a model whose price is known (either from its own listed row
/// or from its family's row) from one with no known price, so unpriced models
/// are never silently reported as costing zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PricedCost {
    /// Priced from the model's own published list-price row, in millionths of
    /// the currency.
    Listed(u64),
    /// Estimated from the model's family row when no dedicated row exists for
    /// this exact model id, in millionths of the currency.
    FamilyEstimate(u64),
    /// No price is known for this model (for example a local model or a
    /// third-party endpoint's open-weight model); the cost is unknown rather
    /// than zero.
    Unknown,
}

impl PricedCost {
    /// The known cost in millionths of the currency, or `None` when the
    /// model's price is unknown.
    #[must_use]
    pub const fn micros(self) -> Option<u64> {
        match self {
            Self::Listed(micros) | Self::FamilyEstimate(micros) => Some(micros),
            Self::Unknown => None,
        }
    }

    /// The listed cost in millionths of the currency when the table carries a
    /// dedicated row for the model itself, else `None`.
    #[must_use]
    pub const fn listed_micros(self) -> Option<u64> {
        match self {
            Self::Listed(micros) => Some(micros),
            Self::FamilyEstimate(_) | Self::Unknown => None,
        }
    }

    /// Whether the model's price is unknown.
    #[must_use]
    pub const fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Whether the price table could price this model (listed or family).
    #[must_use]
    pub const fn is_known(self) -> bool {
        !self.is_unknown()
    }
}

impl std::fmt::Display for PricedCost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listed(micros) => write!(f, "{micros}"),
            Self::FamilyEstimate(micros) => write!(f, "~{micros}"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// What one answer of model `id` costs, distinguishing listed prices, family
/// estimates, and unknown pricing.
#[must_use]
pub fn priced_cost(id: &str, usage: &Usage) -> PricedCost {
    let lowered = id.to_ascii_lowercase();
    let Some(row) = PRICES.iter().find(|row| lowered.contains(row.pattern)) else {
        return PricedCost::Unknown;
    };
    let micros = row.price.of(usage);
    if FAMILIES.contains(&row.pattern) {
        PricedCost::FamilyEstimate(micros)
    } else {
        PricedCost::Listed(micros)
    }
}

/// What one answer of model `id` costs in millionths of the currency, or
/// `None` when no price is known for the model.
///
/// Unlike [`cost_micros`], this distinguishes a known model with zero token
/// usage (`Some(0)`) from an unpriced model (`None`).
#[must_use]
pub fn known_cost_micros(id: &str, usage: &Usage) -> Option<u64> {
    price_of(id).map(|price| price.of(usage))
}

/// Upper-bound cost in millionths of the currency for reserving budget before
/// dispatching a request of `input_tokens` (uncached) and up to
/// `max_output_tokens`, or `None` when no price is known for model `id`.
#[must_use]
pub fn estimated_max_cost_micros(
    id: &str,
    input_tokens: u64,
    max_output_tokens: u64,
) -> Option<u64> {
    let price = price_of(id)?;
    let worst_case = Usage {
        input_tokens,
        output_tokens: max_output_tokens,
        total_tokens: input_tokens.saturating_add(max_output_tokens),
        cached_input_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
    };
    Some(price.of(&worst_case))
}

/// What one answer of model `id` costs, in millionths of the currency: zero
/// for a model no price is known for.
///
/// Callers that need to distinguish unknown model pricing from zero cost
/// should use [`priced_cost`] or [`known_cost_micros`].
#[must_use]
pub fn cost_micros(id: &str, usage: &Usage) -> u64 {
    known_cost_micros(id, usage).unwrap_or(0)
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

    tilo_test_env::scenario! {
        /// The price table applied to a turn's usage.
        fn a_turn_costs_what_its_models_row_charges_for_the_tokens_counted;

        phase a_million_tokens_costs_the_rows_price;
        {
            let price = price_of("claude-sonnet-4-5").expect("a priced model");
            assert_eq!(price.of(&usage(1_000_000, 0, 0, 0)), 3_000_000);
            assert_eq!(price.of(&usage(0, 0, 0, 1_000_000)), 15_000_000);
        }

        phase the_current_claude_models_cost_what_their_provider_lists;
        {
            let million = usage(1_000_000, 0, 0, 0);
            for (model, input, output) in [
                ("claude-fable-5-1", 10_000_000, 50_000_000),
                ("claude-opus-5-5", 4_000_000, 20_000_000),
                ("claude-opus-5", 5_000_000, 25_000_000),
                ("claude-opus-4-8", 5_000_000, 25_000_000),
                ("claude-sonnet-5-5", 2_000_000, 10_000_000),
                ("claude-sonnet-5", 2_000_000, 10_000_000),
                ("claude-sonnet-4-6", 3_000_000, 15_000_000),
                ("claude-haiku-5-5", 100_000, 500_000),
                ("claude-haiku-4-5", 1_000_000, 5_000_000),
            ] {
                assert_eq!(cost_micros(model, &million), input, "{model}");
                assert_eq!(
                    cost_micros(model, &usage(0, 0, 0, 1_000_000)),
                    output,
                    "{model}"
                );
            }
            // A prompt read from the cache costs what each model's row says.
            let cached = usage(1_000_000, 1_000_000, 0, 0);
            assert_eq!(cost_micros("claude-fable-5-1", &cached), 250_000);
            assert_eq!(cost_micros("claude-opus-5-5", &cached), 200_000);
            assert_eq!(cost_micros("claude-sonnet-5-5", &cached), 200_000);
        }

        phase the_most_specific_row_names_the_price;
        {
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

        phase a_cached_prompt_is_charged_what_its_provider_charges;
        {
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

        phase a_model_with_no_price_is_not_charged;
        {
            assert_eq!(
                cost_micros("llama-3.3-70b", &usage(1_000_000, 0, 0, 1_000)),
                0
            );
            assert_eq!(cost_micros("my-local-model", &usage(10, 0, 0, 10)), 0);
            assert!(price_of("llama-3.3-70b").is_none());
        }

        phase a_prompt_never_costs_more_than_its_tokens_are_counted;
        {
            // More cached tokens than the candidate says were used is a broken
            // count, not a windfall: only the counted input is charged.
            let broken = usage(10, 1_000, 1_000, 0);
            assert_eq!(cost_micros("claude-sonnet-4-5", &broken), 3);
            // A reply with no usage counted costs nothing.
            assert_eq!(cost_micros("claude-sonnet-4-5", &Usage::default()), 0);
        }
    }

    /// Only a model's own row is its listed price: a family's row prices a
    /// newer model for the day's bound and says nothing of what it costs.
    #[test]
    fn only_a_models_own_row_is_its_listed_price() {
        for listed in [
            "gemini-2.5-pro",
            "models/gemini-2.5-pro",
            "vendor/gpt-4o-mini",
        ] {
            assert_eq!(listed_price_of(listed), price_of(listed), "{listed}");
            assert!(listed_price_of(listed).is_some(), "{listed}");
        }
        for guessed in [
            "models/gemini-9-ultra",
            "claude-opus-9",
            "grok-7",
            "deepseek-chat",
        ] {
            assert!(listed_price_of(guessed).is_none(), "{guessed}");
            // What a keeper's bound counts by stays as it was.
            assert!(price_of(guessed).is_some(), "{guessed}");
        }
        assert!(listed_price_of("my-local-model").is_none());
    }

    /// Unknown models report `PricedCost::Unknown` / `None` rather than
    /// silently pretending their cost is zero, while a known model with zero
    /// token usage is explicitly `Some(0)`.
    #[test]
    fn unknown_model_pricing_is_distinct_from_zero_cost() {
        let nonempty = usage(1_000_000, 0, 0, 1_000);
        assert_eq!(priced_cost("llama-3.3-70b", &nonempty), PricedCost::Unknown);
        assert_eq!(
            priced_cost("my-local-model", &nonempty),
            PricedCost::Unknown
        );
        assert!(priced_cost("llama-3.3-70b", &nonempty).is_unknown());
        assert_eq!(priced_cost("llama-3.3-70b", &nonempty).micros(), None);
        assert_eq!(
            priced_cost("llama-3.3-70b", &nonempty).to_string(),
            "unknown"
        );
        assert_eq!(known_cost_micros("llama-3.3-70b", &nonempty), None);

        // A known model with zero usage is priced at Some(0), never Unknown.
        assert_eq!(
            priced_cost("claude-opus-5", &Usage::default()),
            PricedCost::Listed(0)
        );
        assert_eq!(
            known_cost_micros("claude-opus-5", &Usage::default()),
            Some(0)
        );

        // Listed models vs family-estimated models are distinguished.
        assert_eq!(
            priced_cost("gemini-2.5-pro", &usage(1_000_000, 0, 0, 0)),
            PricedCost::Listed(1_250_000)
        );
        assert_eq!(
            priced_cost("gemini-2.5-pro", &usage(1_000_000, 0, 0, 0)).listed_micros(),
            Some(1_250_000)
        );
        assert_eq!(
            priced_cost("claude-sonnet-4-5", &usage(1_000_000, 0, 0, 0)),
            PricedCost::FamilyEstimate(3_000_000)
        );
        assert_eq!(
            priced_cost("claude-sonnet-4-5", &usage(1_000_000, 0, 0, 0)).listed_micros(),
            None
        );
        assert_eq!(
            priced_cost("claude-sonnet-4-5", &usage(1_000_000, 0, 0, 0)).micros(),
            Some(3_000_000)
        );

        // Pre-call upper-bound estimates distinguish known vs unknown models.
        assert_eq!(
            estimated_max_cost_micros("claude-sonnet-5", 1_000_000, 1_000_000),
            Some(12_000_000)
        );
        assert_eq!(
            estimated_max_cost_micros("my-local-model", 1_000_000, 1_000_000),
            None
        );
    }
}
