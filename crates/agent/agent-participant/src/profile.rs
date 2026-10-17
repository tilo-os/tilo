//! Opt-in request preferences for host-side OpenAI-compatible planners.
//!
//! These are provider hints, not permissions, latency guarantees or price
//! estimates. No preset is selected by default. Explicit settings override
//! presets; unsupported settings may be refused by the configured provider.
//! No preset duplicates requests or falls back to a more expensive tier.

/// Select `fastest`, `balanced` or `cheapest` for a host-side planner.
pub const PRESET_ENV: &str = "TILO_AGENT_PRESET";
/// Override the model-name heuristic with `small`, `medium`, `large` or `very-large`.
pub const MODEL_CLASS_ENV: &str = "TILO_AGENT_MODEL_CLASS";
/// Override a preset's service tier.
pub const SERVICE_TIER_ENV: &str = "TILO_AGENT_SERVICE_TIER";
/// Override a preset's reasoning effort.
pub const REASONING_EFFORT_ENV: &str = "TILO_AGENT_REASONING_EFFORT";

/// A coarse model-family hint, overridable for provider aliases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelClass {
    /// Small model families.
    Small,
    /// The fallback for unrecognised model names.
    Medium,
    /// Large model families.
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

/// Provider request hints only; neither field changes agent authority.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    service_tier: Option<String>,
    reasoning_effort: Option<String>,
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
        if let Some(tier) = explicit(SERVICE_TIER_ENV) {
            profile.service_tier = recognised(&tier, &["auto", "default", "flex", "priority"]);
        }
        if let Some(effort) = explicit(REASONING_EFFORT_ENV) {
            profile.reasoning_effort = recognised(
                &effort,
                &["none", "minimal", "low", "medium", "high", "xhigh"],
            );
        }
        profile
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
}
