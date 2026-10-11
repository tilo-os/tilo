//! What a planning model's size asks of the agent: presets of service tier,
//! reasoning effort and hedging, by model class and by what the person
//! running the agent wants most (speed, balance, cost).
//!
//! The presets come from the campaign benchmark (`tests/campaign_bench.rs`):
//! a small model answers a call in one to three seconds, so a call still
//! running after three and a half is in the provider's latency tail and is
//! worth sending again at a fraction of a cent; a medium one's tail is too
//! close to its median for a resend to help; a very large one takes three to
//! five, its calls are too dear to double, and its Flex tier answered as
//! fast as standard at half the price. Any setting given outright ([`SERVICE_TIER_ENV`],
//! [`REASONING_EFFORT_ENV`], [`HEDGE_ENV`]) overrides the preset's.
//!
//! [`SERVICE_TIER_ENV`]: crate::model::SERVICE_TIER_ENV
//! [`REASONING_EFFORT_ENV`]: crate::model::REASONING_EFFORT_ENV
//! [`HEDGE_ENV`]: crate::model::HEDGE_ENV

use std::time::Duration;

/// The environment variable naming the preset ([`Goal`]): `fastest`,
/// `balanced` or `cheapest`.
pub const PRESET_ENV: &str = "TILO_AGENT_PRESET";

/// The environment variable that overrides the class a model's name implies
/// ([`ModelClass`]): `small`, `medium`, `large` or `very-large`.
pub const MODEL_CLASS_ENV: &str = "TILO_AGENT_MODEL_CLASS";

/// How large a planning model is, as its latency and price follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelClass {
    /// About $0.10 per million input tokens; one to three seconds a call
    /// (GPT-6 Luna, Claude Haiku).
    Small,
    /// About $2 per million input tokens (GPT-6 Sol, GPT-5.6 Terra, Claude
    /// Sonnet).
    Medium,
    /// About $4–5 per million input tokens (Claude Opus).
    Large,
    /// About $10 per million input tokens; three to five seconds a call
    /// (GPT-6 Astra, Claude Fable).
    VeryLarge,
}

impl ModelClass {
    /// The class of the model named `model`: by family name, `medium` for
    /// a name none matches.
    #[must_use]
    pub fn of(model: &str) -> Self {
        let name = model.to_ascii_lowercase();
        let named = |families: &[&str]| families.iter().any(|family| name.contains(family));
        if named(&["luna", "haiku", "nano", "mini", "flash-lite"]) {
            Self::Small
        } else if named(&["astra", "fable", "mythos", "-pro", "ultra"]) {
            Self::VeryLarge
        } else if named(&["opus"]) {
            Self::Large
        } else {
            Self::Medium
        }
    }

    /// The class `text` names (`small`, `medium`, `large`, `very-large`).
    #[must_use]
    pub fn named(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "large" => Some(Self::Large),
            "very-large" | "verylarge" | "xlarge" => Some(Self::VeryLarge),
            _ => None,
        }
    }
}

/// What a preset puts first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    /// The shortest run: the provider's fast tier, low effort, hedged calls
    /// where a model is cheap enough to send twice.
    Fastest,
    /// Low effort at the standard tier (or Flex, where it is no slower).
    Balanced,
    /// The cheapest run: the Flex tier at half price, low effort.
    Cheapest,
}

impl Goal {
    /// The goal `text` names (`fastest`, `balanced`, `cheapest`).
    #[must_use]
    pub fn named(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "fastest" | "fast" => Some(Self::Fastest),
            "balanced" | "balance" => Some(Self::Balanced),
            "cheapest" | "cheap" => Some(Self::Cheapest),
            _ => None,
        }
    }
}

/// Which settings a provider takes: an `OpenAI` model takes service tiers,
/// reasoning efforts down to `low` on the GPT-6 family; Anthropic's take
/// neither tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// `OpenAI`'s Responses or chat-completions API.
    OpenAi,
    /// Anthropic's Messages API.
    Anthropic,
}

/// A planning model's settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    /// The service tier asked for (`flex`, `priority`); `None` the default.
    pub service_tier: Option<String>,
    /// The reasoning effort asked for; `None` the model's default.
    pub reasoning_effort: Option<String>,
    /// After how long a call is sent again; `None` never.
    pub hedge_after: Option<Duration>,
}

impl Profile {
    /// The preset for a model of `class` behind `provider` that puts `goal`
    /// first.
    #[must_use]
    pub fn preset(class: ModelClass, goal: Goal, provider: Provider) -> Self {
        let (tier, hedge_ms) = match (class, goal) {
            (ModelClass::Small, Goal::Fastest) => (Some("priority"), Some(3_500)),
            // A medium or large model's tail is close to its median call:
            // a hedge resends ordinary calls and bought nothing on GPT-6 Sol,
            // GPT-6.1 Sol or GPT-5.6 Terra.
            (ModelClass::Medium | ModelClass::Large, Goal::Fastest) => (Some("priority"), None),
            // Flex answered a very large model as fast as priority and
            // standard did, at a quarter and half the price.
            (ModelClass::VeryLarge, Goal::Fastest | Goal::Balanced) | (_, Goal::Cheapest) => {
                (Some("flex"), None)
            }
            (_, Goal::Balanced) => (None, None),
        };
        let tier = match provider {
            Provider::OpenAi => tier.map(str::to_owned),
            Provider::Anthropic => None,
        };
        Self {
            service_tier: tier,
            reasoning_effort: Some("low".to_owned()),
            hedge_after: hedge_ms.map(Duration::from_millis),
        }
    }

    /// The profile of the model named `model` behind `provider`: the preset
    /// [`PRESET_ENV`] names for its class (or [`MODEL_CLASS_ENV`]'s), then
    /// each setting given outright through `explicit` instead.
    #[must_use]
    pub fn resolve(
        model: &str,
        provider: Provider,
        explicit: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let class = explicit(MODEL_CLASS_ENV)
            .as_deref()
            .and_then(ModelClass::named)
            .unwrap_or_else(|| ModelClass::of(model));
        let mut profile = explicit(PRESET_ENV)
            .as_deref()
            .and_then(Goal::named)
            .map(|goal| Self::preset(class, goal, provider))
            .unwrap_or_default();
        if let Some(tier) = explicit(crate::model::SERVICE_TIER_ENV) {
            profile.service_tier = Some(tier);
        }
        if let Some(effort) = explicit(crate::model::REASONING_EFFORT_ENV) {
            profile.reasoning_effort = Some(effort);
        }
        if let Some(hedge) = explicit(crate::model::HEDGE_ENV) {
            // `0` or anything not a number (`off`) turns the preset's
            // hedging off.
            profile.hedge_after = hedge
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|ms| *ms > 0)
                .map(Duration::from_millis);
        }
        profile
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_is_classed_by_its_family() {
        assert_eq!(ModelClass::of("gpt-6-luna"), ModelClass::Small);
        assert_eq!(ModelClass::of("claude-haiku-5-5"), ModelClass::Small);
        assert_eq!(ModelClass::of("gpt-5.6-terra"), ModelClass::Medium);
        assert_eq!(ModelClass::of("gpt-6.1-sol"), ModelClass::Medium);
        assert_eq!(ModelClass::of("claude-sonnet-5-5"), ModelClass::Medium);
        assert_eq!(ModelClass::of("claude-opus-5-5"), ModelClass::Large);
        assert_eq!(ModelClass::of("gpt-6-astra"), ModelClass::VeryLarge);
        assert_eq!(ModelClass::of("claude-fable-5-1"), ModelClass::VeryLarge);
        assert_eq!(ModelClass::of("some-local-model"), ModelClass::Medium);
    }

    #[test]
    fn a_preset_follows_class_and_goal_and_what_the_provider_takes() {
        let fastest = Profile::preset(ModelClass::Small, Goal::Fastest, Provider::OpenAi);
        assert_eq!(fastest.service_tier.as_deref(), Some("priority"));
        assert_eq!(fastest.hedge_after, Some(Duration::from_millis(3_500)));
        let cheapest = Profile::preset(ModelClass::Small, Goal::Cheapest, Provider::OpenAi);
        assert_eq!(cheapest.service_tier.as_deref(), Some("flex"));
        assert_eq!(cheapest.hedge_after, None);
        let astra = Profile::preset(ModelClass::VeryLarge, Goal::Fastest, Provider::OpenAi);
        assert_eq!(astra.service_tier.as_deref(), Some("flex"));
        assert_eq!(astra.hedge_after, None);
        let claude = Profile::preset(ModelClass::Small, Goal::Fastest, Provider::Anthropic);
        assert_eq!(claude.service_tier, None);
        assert_eq!(claude.reasoning_effort.as_deref(), Some("low"));
    }

    #[test]
    fn a_setting_given_outright_overrides_the_preset() {
        let given = |name: &str| match name {
            PRESET_ENV => Some("fastest".to_owned()),
            crate::model::HEDGE_ENV => Some("4000".to_owned()),
            crate::model::SERVICE_TIER_ENV => Some("flex".to_owned()),
            _ => None,
        };
        let profile = Profile::resolve("gpt-5.6-terra", Provider::OpenAi, &given);
        assert_eq!(profile.service_tier.as_deref(), Some("flex"));
        assert_eq!(profile.hedge_after, Some(Duration::from_millis(4_000)));
        assert_eq!(profile.reasoning_effort.as_deref(), Some("low"));
        let unhedged = |name: &str| match name {
            PRESET_ENV => Some("fastest".to_owned()),
            crate::model::HEDGE_ENV => Some("0".to_owned()),
            _ => None,
        };
        let profile = Profile::resolve("gpt-6-luna", Provider::OpenAi, &unhedged);
        assert_eq!(profile.service_tier.as_deref(), Some("priority"));
        assert_eq!(profile.hedge_after, None);
        let none = Profile::resolve("gpt-6-luna", Provider::OpenAi, &|_| None);
        assert_eq!(none, Profile::default());
    }
}
