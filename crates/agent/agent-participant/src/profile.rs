//! Opt-in request preferences for host-side OpenAI-compatible planners.
//!
//! These are provider hints, not permissions, latency guarantees or price
//! estimates. No preset is selected by default. Explicit settings override
//! presets; unsupported settings may be refused by the configured provider.
//! No preset duplicates requests or falls back to a more expensive tier:
//! a call is sent again only after [`HEDGE_ENV`] says how long to wait.
//!
//! Besides provider hints, a profile says whether a finished task is
//! checked against its request ([`CHECK_WORK_ENV`]): small models skip or
//! get wrong a part of a long request most often, so their presets check.

use tilo_model_gateway::host_backend::TokenLimitParam;

/// Select `fastest`, `balanced` or `cheapest` for a host-side planner.
pub const PRESET_ENV: &str = "TILO_AGENT_PRESET";
/// Override the model-name heuristic with `small`, `medium`, `large` or `very-large`.
pub const MODEL_CLASS_ENV: &str = "TILO_AGENT_MODEL_CLASS";
/// Override a preset's service tier.
pub const SERVICE_TIER_ENV: &str = "TILO_AGENT_SERVICE_TIER";
/// Override a preset's reasoning effort.
pub const REASONING_EFFORT_ENV: &str = "TILO_AGENT_REASONING_EFFORT";
/// Turn the check of a finished task against its request on (`on`, `1`)
/// or off (anything else), whatever the preset says.
pub const CHECK_WORK_ENV: &str = "TILO_AGENT_CHECK_WORK";
/// Send a planning call again when it has not answered within this many
/// milliseconds, taking whichever answers first; `0` or unset never does.
pub const HEDGE_ENV: &str = "TILO_AGENT_HEDGE_MS";
/// Name an OpenAI-compatible endpoint's output limit `max_tokens` or
/// `max_completion_tokens`, whatever the model's family suggests.
pub const TOKEN_LIMIT_ENV: &str = "TILO_AGENT_TOKEN_LIMIT_PARAM";

/// A coarse model-family hint, overridable for provider aliases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelClass {
    /// Small model families.
    Small,
    /// The fallback for unrecognised model names.
    Medium,
    Large,
    /// Very large model families.
    VeryLarge,
}

impl ModelClass {
    /// Infer a class from a model's family name, not its capabilities or price.
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

    /// Parse an operator's class override.
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

/// Which request preference to prioritise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    /// Request priority service, except for very large models.
    Fastest,
    /// Request the provider's default tier, except for very large models.
    Balanced,
    /// Request Flex service.
    Cheapest,
}

impl Goal {
    /// Parse a preset name.
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

/// Provider request hints, whether finished work is checked and when a
/// slow call is sent again; none of them changes agent authority.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    service_tier: Option<String>,
    reasoning_effort: Option<String>,
    /// Whether a task's result is checked against its request once its
    /// steps ran: one more planning turn, which fixes what is missing.
    check_work: bool,
    /// After how long a call is sent again ([`HEDGE_ENV`]); `None` never.
    hedge_after: Option<std::time::Duration>,
    /// The output limit's field ([`TOKEN_LIMIT_ENV`]); `None` sends the
    /// request's own (`max_tokens`).
    token_limit: Option<TokenLimitParam>,
}

impl Profile {
    /// Resolve opt-in preferences without reading process-global state.
    /// An explicit invalid setting suppresses that hint rather than sending
    /// an unrecognised value or silently restoring the preset's value.
    #[must_use]
    pub fn resolve(model: &str, explicit: &dyn Fn(&str) -> Option<String>) -> Self {
        let class = explicit(MODEL_CLASS_ENV)
            .as_deref()
            .and_then(ModelClass::named)
            .unwrap_or_else(|| ModelClass::of(model));
        let mut profile = explicit(PRESET_ENV)
            .as_deref()
            .and_then(Goal::named)
            .map(|goal| Self::preset(class, goal))
            .unwrap_or_default();
        profile.token_limit = match explicit(TOKEN_LIMIT_ENV) {
            Some(named) => match named.trim().to_ascii_lowercase().as_str() {
                "max_tokens" => Some(TokenLimitParam::MaxTokens),
                "max_completion_tokens" => Some(TokenLimitParam::MaxCompletionTokens),
                _ => None,
            },
            None => reasoning_family(model).then_some(TokenLimitParam::MaxCompletionTokens),
        };
        if let Some(tier) = explicit(SERVICE_TIER_ENV) {
            profile.service_tier = recognised(&tier, &["auto", "default", "flex", "priority"]);
        }
        if let Some(effort) = explicit(REASONING_EFFORT_ENV) {
            profile.reasoning_effort = recognised(
                &effort,
                &["none", "minimal", "low", "medium", "high", "xhigh"],
            );
        }
        if let Some(check) = explicit(CHECK_WORK_ENV) {
            profile.check_work = matches!(
                check.trim().to_ascii_lowercase().as_str(),
                "1" | "on" | "true" | "yes"
            );
        }
        if let Some(hedge) = explicit(HEDGE_ENV) {
            // `0` or anything not a number (`off`) leaves calls unhedged.
            profile.hedge_after = hedge
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|ms| *ms > 0)
                .map(std::time::Duration::from_millis);
        }
        profile
    }

    /// Whether a finished task is checked against its request.
    #[must_use]
    pub const fn checks_work(&self) -> bool {
        self.check_work
    }

    /// After how long a planning call is sent again, if ever.
    #[must_use]
    pub const fn hedge_after(&self) -> Option<std::time::Duration> {
        self.hedge_after
    }

    /// The field an OpenAI-compatible endpoint takes its output limit in,
    /// when it is not the request's own `max_tokens`.
    #[must_use]
    pub const fn token_limit_param(&self) -> Option<TokenLimitParam> {
        self.token_limit
    }

    /// The service tier asked for, if any.
    #[must_use]
    pub fn service_tier(&self) -> Option<&str> {
        self.service_tier.as_deref()
    }

    /// The class/goal policy. Very large models request Flex for every goal;
    /// all presets request low reasoning effort. Provider support varies.
    #[must_use]
    pub fn preset(class: ModelClass, goal: Goal) -> Self {
        let tier = match (class, goal) {
            (ModelClass::VeryLarge, _) | (_, Goal::Cheapest) => Some("flex"),
            (_, Goal::Fastest) => Some("priority"),
            (_, Goal::Balanced) => None,
        };
        Self {
            service_tier: tier.map(str::to_owned),
            reasoning_effort: Some("low".to_owned()),
            // A small model skips or gets wrong a part of a long request
            // most often. The check is one or two more planning turns on
            // the task's critical path (a medium model's run took three
            // times as long), so larger models skip it unless asked.
            check_work: class == ModelClass::Small,
            hedge_after: None,
            token_limit: None,
        }
    }

    pub(crate) fn apply(&self, body: &mut serde_json::Value) {
        if let Some(tier) = &self.service_tier {
            body["service_tier"] = tier.clone().into();
        }
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = effort.clone().into();
        }
    }
}

/// Whether `model` is of an `OpenAI` reasoning family, whose models take
/// their output limit as `max_completion_tokens` and refuse `max_tokens`.
fn reasoning_family(model: &str) -> bool {
    let name = model.trim().to_ascii_lowercase();
    ["gpt-5", "gpt-6", "gpt-7", "o1", "o3", "o4"]
        .iter()
        .any(|family| name.starts_with(family))
}

fn recognised(text: &str, accepted: &[&str]) -> Option<String> {
    let text = text.trim().to_ascii_lowercase();
    accepted.contains(&text.as_str()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_names_and_operator_overrides_are_case_insensitive() {
        assert_eq!(ModelClass::of("GPT-mini"), ModelClass::Small);
        assert_eq!(ModelClass::of("claude-opus"), ModelClass::Large);
        assert_eq!(ModelClass::of("model-pro"), ModelClass::VeryLarge);
        assert_eq!(ModelClass::of("local-alias"), ModelClass::Medium);
        assert_eq!(
            ModelClass::named(" VERY_LARGE "),
            Some(ModelClass::VeryLarge)
        );
        assert_eq!(ModelClass::named("unknown"), None);
        assert_eq!(Goal::named(" FAST "), Some(Goal::Fastest));
        assert_eq!(Goal::named("unknown"), None);
    }

    #[test]
    fn reasoning_families_name_their_output_limit_unless_told_otherwise() {
        let none = |_: &str| None;
        assert_eq!(
            Profile::resolve("gpt-6-astra", &none).token_limit_param(),
            Some(TokenLimitParam::MaxCompletionTokens)
        );
        assert_eq!(
            Profile::resolve("claude-opus-5-5", &none).token_limit_param(),
            None
        );
        assert_eq!(Profile::resolve("llama-4", &none).token_limit_param(), None);
        let told = |name: &str| (name == TOKEN_LIMIT_ENV).then(|| " MAX_TOKENS ".to_owned());
        assert_eq!(
            Profile::resolve("gpt-6-astra", &told).token_limit_param(),
            Some(TokenLimitParam::MaxTokens)
        );
    }

    #[test]
    fn every_class_and_goal_has_the_expected_hints() {
        for class in [
            ModelClass::Small,
            ModelClass::Medium,
            ModelClass::Large,
            ModelClass::VeryLarge,
        ] {
            for goal in [Goal::Fastest, Goal::Balanced, Goal::Cheapest] {
                let profile = Profile::preset(class, goal);
                let tier = if class == ModelClass::VeryLarge || goal == Goal::Cheapest {
                    Some("flex")
                } else if goal == Goal::Fastest {
                    Some("priority")
                } else {
                    None
                };
                assert_eq!(profile.service_tier.as_deref(), tier);
                assert_eq!(profile.reasoning_effort.as_deref(), Some("low"));
                assert_eq!(profile.checks_work(), class == ModelClass::Small);
                assert_eq!(profile.hedge_after(), None, "no preset hedges");
            }
        }
    }

    #[test]
    fn explicit_hints_work_without_a_preset() {
        let profile = Profile::resolve("local-alias", &|name| match name {
            SERVICE_TIER_ENV => Some(" FLEX ".into()),
            REASONING_EFFORT_ENV => Some("none".into()),
            _ => None,
        });
        assert_eq!(profile.service_tier.as_deref(), Some("flex"));
        assert_eq!(profile.reasoning_effort.as_deref(), Some("none"));
        let invalid = Profile::resolve("local-alias", &|name| match name {
            SERVICE_TIER_ENV | REASONING_EFFORT_ENV => Some("invalid".into()),
            _ => None,
        });
        assert_eq!(invalid, Profile::default());
    }

    #[test]
    fn explicit_values_override_presets_and_invalid_values_suppress_hints() {
        let profile = Profile::resolve("mini", &|name| match name {
            PRESET_ENV => Some("fastest".into()),
            MODEL_CLASS_ENV => Some("very-large".into()),
            REASONING_EFFORT_ENV => Some(" HIGH ".into()),
            _ => None,
        });
        assert_eq!(profile.service_tier.as_deref(), Some("flex"));
        assert_eq!(profile.reasoning_effort.as_deref(), Some("high"));
        let profile = Profile::resolve("mini", &|name| match name {
            PRESET_ENV => Some("fastest".into()),
            SERVICE_TIER_ENV => Some(" DEFAULT ".into()),
            REASONING_EFFORT_ENV => Some("off".into()),
            _ => None,
        });
        assert_eq!(profile.service_tier.as_deref(), Some("default"));
        assert_eq!(profile.reasoning_effort, None);
        assert_eq!(Profile::resolve("mini", &|_| None), Profile::default());
        assert_eq!(
            Profile::resolve("mini", &|_| Some("invalid".into())),
            Profile::default()
        );
    }

    #[test]
    fn the_work_check_and_hedging_are_switched_outright() {
        let small = |name: &str| match name {
            PRESET_ENV => Some("fastest".into()),
            _ => None,
        };
        assert!(Profile::resolve("gpt-mini", &small).checks_work());
        let off = Profile::resolve("gpt-mini", &|name| match name {
            PRESET_ENV => Some("fastest".into()),
            CHECK_WORK_ENV => Some("off".into()),
            _ => None,
        });
        assert!(!off.checks_work());
        let on = Profile::resolve("local-alias", &|name| match name {
            CHECK_WORK_ENV => Some(" ON ".into()),
            HEDGE_ENV => Some("3500".into()),
            _ => None,
        });
        assert!(on.checks_work());
        assert_eq!(
            on.hedge_after(),
            Some(std::time::Duration::from_millis(3_500))
        );
        for unhedged in ["0", "off", ""] {
            let profile = Profile::resolve("local-alias", &|name| {
                (name == HEDGE_ENV).then(|| unhedged.to_owned())
            });
            assert_eq!(profile.hedge_after(), None, "{unhedged:?}");
        }
    }
}
