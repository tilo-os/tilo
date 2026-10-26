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

use tilo_model_gateway::host_backend::{OpenAiApi, TokenLimitParam};

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
/// Carry each item of a task's checklist out with an agent of its own,
/// a coordinator listing the items and following news for them: `on`
/// (`1`) for every agent, or a comma-separated list of agent ids or labels
/// for those agents alone ([`named_on`]).
pub const DECOMPOSE_ENV: &str = "TILO_AGENT_DECOMPOSE";
/// Plan the items of a coordinated task ([`DECOMPOSE_ENV`]) with this
/// model, while the coordinator (the list, news, an item not done, the
/// check) plans with the agent's own: a model id, asked at Anthropic's
/// Messages API when it is a `claude-` one and at `OpenAI`'s otherwise,
/// unless [`DECOMPOSE_ITEM_URL_ENV`] names the endpoint. Unset, the items
/// plan with the agent's own model.
pub const DECOMPOSE_ITEM_MODEL_ENV: &str = "TILO_AGENT_DECOMPOSE_ITEM_MODEL";
/// The endpoint of [`DECOMPOSE_ITEM_MODEL_ENV`], as the agent's own model's
/// is named: `openai:<base url>`, `anthropic:<base url>` or
/// `cloud:<base url>`.
pub const DECOMPOSE_ITEM_URL_ENV: &str = "TILO_AGENT_DECOMPOSE_ITEM_URL";
/// The key of [`DECOMPOSE_ITEM_MODEL_ENV`]: the key itself.
pub const DECOMPOSE_ITEM_API_KEY_ENV: &str = "TILO_AGENT_DECOMPOSE_ITEM_API_KEY";
/// The key of [`DECOMPOSE_ITEM_MODEL_ENV`]: a file holding it, read before
/// each call. One of the two is needed unless [`DECOMPOSE_ITEM_URL_ENV`]
/// names an `openai:` endpoint, which may take none.
pub const DECOMPOSE_ITEM_API_KEY_FILE_ENV: &str = "TILO_AGENT_DECOMPOSE_ITEM_API_KEY_FILE";
/// The preset of [`DECOMPOSE_ITEM_MODEL_ENV`], in place of [`PRESET_ENV`]:
/// the item model's provider may serve another tier than the agent's own.
pub const DECOMPOSE_ITEM_PRESET_ENV: &str = "TILO_AGENT_DECOMPOSE_ITEM_PRESET";
/// Ask an OpenAI-compatible endpoint through `responses` or
/// `chat_completions`, whatever its address suggests (a proxy in front of
/// `OpenAI` takes Responses); unset, `OpenAI`'s own API takes Responses and
/// any other endpoint chat completions.
pub const OPENAI_API_ENV: &str = "TILO_AGENT_OPENAI_API";

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
    /// The API asked through ([`OPENAI_API_ENV`]); `None` as the endpoint
    /// suggests.
    openai_api: Option<OpenAiApi>,
    /// Whether every agent's tasks are coordinated, an agent per item
    /// ([`DECOMPOSE_ENV`]); a setting naming agents turns it on for them
    /// where each starts.
    decompose: bool,
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
        profile.openai_api = explicit(OPENAI_API_ENV).and_then(|named| {
            match named.trim().to_ascii_lowercase().replace('-', "_").as_str() {
                "responses" => Some(OpenAiApi::Responses),
                "chat_completions" | "chat" => Some(OpenAiApi::ChatCompletions),
                _ => None,
            }
        });
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
        profile.decompose = matches!(
            explicit(DECOMPOSE_ENV)
                .map(|setting| setting.trim().to_ascii_lowercase())
                .as_deref(),
            Some("1" | "on" | "true" | "yes" | "all")
        );
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

    /// The profile of the model a coordinated task's items plan with
    /// ([`DECOMPOSE_ITEM_MODEL_ENV`]): [`DECOMPOSE_ITEM_PRESET_ENV`] in place
    /// of [`PRESET_ENV`] when set, and the agent's own model's class, output
    /// limit and API left out, as they describe that model and its endpoint.
    /// The other settings are the agent's own.
    #[must_use]
    pub fn resolve_items(model: &str, explicit: &dyn Fn(&str) -> Option<String>) -> Self {
        Self::resolve(model, &|name| match name {
            PRESET_ENV => explicit(DECOMPOSE_ITEM_PRESET_ENV).or_else(|| explicit(PRESET_ENV)),
            MODEL_CLASS_ENV | TOKEN_LIMIT_ENV | OPENAI_API_ENV => None,
            _ => explicit(name),
        })
    }

    /// Whether a finished task is checked against its request.
    #[must_use]
    pub const fn checks_work(&self) -> bool {
        self.check_work
    }

    /// Whether every agent's tasks are coordinated, an agent per item.
    #[must_use]
    pub const fn decomposes(&self) -> bool {
        self.decompose
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

    /// The API an OpenAI-compatible endpoint is asked through, when it was
    /// named.
    #[must_use]
    pub const fn openai_api(&self) -> Option<OpenAiApi> {
        self.openai_api
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
            openai_api: None,
            decompose: false,
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

/// Whether a per-agent `setting` turns a feature on for the agent `agent_id`
/// labelled `label`: on for all (`1`, `on`, `true`, `yes`, `all`), off for
/// all (`0`, `off`, `false`, `no`, `none`, unset), or a comma-separated list
/// of the agent ids and labels it is on for.
#[must_use]
pub fn named_on(agent_id: &str, label: &str, setting: Option<&str>) -> bool {
    let Some(setting) = setting.map(str::trim).filter(|setting| !setting.is_empty()) else {
        return false;
    };
    match setting.to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" | "all" => true,
        "0" | "off" | "false" | "no" | "none" => false,
        _ => setting.split(',').map(str::trim).any(|named| {
            !named.is_empty() && (named == agent_id || named.eq_ignore_ascii_case(label))
        }),
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
    fn the_openai_api_is_asked_through_only_when_named() {
        let none = |_: &str| None;
        assert_eq!(Profile::resolve("gpt-6-luna", &none).openai_api(), None);
        for (named, api) in [
            (" Responses ", Some(OpenAiApi::Responses)),
            ("chat-completions", Some(OpenAiApi::ChatCompletions)),
            ("chat", Some(OpenAiApi::ChatCompletions)),
            ("graphql", None),
        ] {
            let told = |name: &str| (name == OPENAI_API_ENV).then(|| named.to_owned());
            assert_eq!(
                Profile::resolve("gpt-6-luna", &told).openai_api(),
                api,
                "{named}"
            );
        }
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

    /// The item model's profile takes its own preset, else the agent's,
    /// and its own class and output limit, never the agent's model's.
    #[test]
    fn the_item_model_takes_its_own_preset_and_class() {
        let told = |item_preset: Option<&'static str>| {
            move |name: &str| match name {
                PRESET_ENV => Some("fastest".to_owned()),
                MODEL_CLASS_ENV => Some("very-large".to_owned()),
                TOKEN_LIMIT_ENV => Some("max_tokens".to_owned()),
                DECOMPOSE_ITEM_PRESET_ENV => item_preset.map(str::to_owned),
                _ => None,
            }
        };
        // The agent's own model: its class override makes `fastest` Flex.
        let own = Profile::resolve("gpt-6.1-terra", &told(None));
        assert_eq!(own.service_tier(), Some("flex"));
        // The items' small model, under the agent's preset: priority, as
        // its own class says.
        let items = Profile::resolve_items("gpt-6-luna", &told(None));
        assert_eq!(items.service_tier(), Some("priority"));
        assert_eq!(
            items.token_limit_param(),
            Some(TokenLimitParam::MaxCompletionTokens)
        );
        let cheapest = Profile::resolve_items("gpt-6-luna", &told(Some("cheapest")));
        assert_eq!(cheapest.service_tier(), Some("flex"));
        assert_eq!(
            Profile::resolve_items("claude-haiku-5-5", &told(Some("balanced"))).token_limit_param(),
            None
        );
    }

    /// Every agent's tasks are coordinated when the setting turns it on;
    /// a setting naming agents turns it on for them alone, where each
    /// starts, so the profile leaves it off.
    #[test]
    fn decompose_is_on_for_all_or_for_the_agents_named() {
        let told = |value: &'static str| {
            move |name: &str| (name == DECOMPOSE_ENV).then(|| value.to_owned())
        };
        assert!(!Profile::resolve("gpt-6-luna", &|_| None).decomposes());
        assert!(Profile::resolve("gpt-6-luna", &told(" ON ")).decomposes());
        assert!(!Profile::resolve("gpt-6-luna", &told("ana-1")).decomposes());
        assert!(named_on("ana-1", "Ana", Some("ben-2, ana-1")));
        assert!(named_on("ana-1", "Ana", Some("ana")));
        assert!(!named_on("ana-1", "Ana", Some("ben-2")));
        assert!(!named_on("ana-1", "Ana", Some("off")));
        assert!(!named_on("ana-1", "Ana", None));
    }
}
