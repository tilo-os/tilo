//! Typed per-model request options: fast mode and reasoning effort.
//!
//! A model declares which options it supports ([`ModelOptionSupport`]); an
//! assignment chooses among them ([`ModelOptions`]); the gateway writes them
//! into the provider's own dialect ([`ModelOptions::apply`]). No free-form
//! parameter bag crosses any of these boundaries.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ProviderProtocol;

/// How much a reasoning model deliberates before it answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    /// The efforts `protocol` defines, in increasing order.
    #[must_use]
    pub const fn for_protocol(protocol: ProviderProtocol) -> &'static [Self] {
        match protocol {
            ProviderProtocol::Anthropic => {
                &[Self::Low, Self::Medium, Self::High, Self::XHigh, Self::Max]
            }
            ProviderProtocol::OpenAiCompatible | ProviderProtocol::AzureOpenAi => &[
                Self::Minimal,
                Self::Low,
                Self::Medium,
                Self::High,
                Self::XHigh,
            ],
        }
    }

    /// The wire value both dialects use.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Fluent key of the effort's name.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Minimal => "ai-effort-minimal",
            Self::Low => "ai-effort-low",
            Self::Medium => "ai-effort-medium",
            Self::High => "ai-effort-high",
            Self::XHigh => "ai-effort-xhigh",
            Self::Max => "ai-effort-max",
        }
    }
}

/// Which options one model accepts. Declared with the model, never guessed
/// from its name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelOptionSupport {
    /// The provider's premium low-latency tier (Anthropic fast mode, `OpenAI`
    /// and Azure priority processing).
    pub fast_mode: bool,
    /// Whether the model takes a reasoning effort. The efforts offered are
    /// the protocol's ([`ReasoningEffort::for_protocol`]).
    pub reasoning: bool,
}

/// The options an assignment runs a model with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelOptions {
    pub fast_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// An option the model or protocol does not accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionError {
    FastModeUnsupported,
    ReasoningUnsupported,
    EffortUnsupported,
}

impl ModelOptions {
    /// Check these options against a model's declared support.
    ///
    /// # Errors
    ///
    /// Returns the first option the model or its protocol does not accept.
    pub fn validate(
        &self,
        protocol: ProviderProtocol,
        support: &ModelOptionSupport,
    ) -> Result<(), OptionError> {
        if self.fast_mode && !support.fast_mode {
            return Err(OptionError::FastModeUnsupported);
        }
        if let Some(effort) = self.reasoning_effort {
            if !support.reasoning {
                return Err(OptionError::ReasoningUnsupported);
            }
            if !ReasoningEffort::for_protocol(protocol).contains(&effort) {
                return Err(OptionError::EffortUnsupported);
            }
        }
        Ok(())
    }

    /// Write these options into a request `body` of `protocol`'s dialect and
    /// return the extra headers the request needs.
    pub fn apply(
        &self,
        protocol: ProviderProtocol,
        body: &mut Value,
    ) -> Vec<(&'static str, &'static str)> {
        let mut headers = Vec::new();
        let Some(object) = body.as_object_mut() else {
            return headers;
        };
        match protocol {
            ProviderProtocol::Anthropic => {
                if self.fast_mode {
                    object.insert("speed".into(), Value::String("fast".into()));
                    headers.push(("anthropic-beta", "fast-mode-2026-02-01"));
                }
                if let Some(effort) = self.reasoning_effort {
                    let config = object
                        .entry("output_config")
                        .or_insert_with(|| Value::Object(serde_json::Map::new()));
                    if let Some(config) = config.as_object_mut() {
                        config.insert("effort".into(), Value::String(effort.wire().into()));
                    }
                }
            }
            ProviderProtocol::OpenAiCompatible | ProviderProtocol::AzureOpenAi => {
                if self.fast_mode {
                    object.insert("service_tier".into(), Value::String("priority".into()));
                }
                if let Some(effort) = self.reasoning_effort {
                    object.insert(
                        "reasoning_effort".into(),
                        Value::String(effort.wire().into()),
                    );
                }
            }
        }
        headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_checked_against_the_declared_support() {
        let fast = ModelOptions {
            fast_mode: true,
            reasoning_effort: None,
        };
        assert_eq!(
            fast.validate(
                ProviderProtocol::AzureOpenAi,
                &ModelOptionSupport::default()
            ),
            Err(OptionError::FastModeUnsupported)
        );
        let support = ModelOptionSupport {
            fast_mode: true,
            reasoning: true,
        };
        assert_eq!(
            fast.validate(ProviderProtocol::AzureOpenAi, &support),
            Ok(())
        );
        let max = ModelOptions {
            fast_mode: false,
            reasoning_effort: Some(ReasoningEffort::Max),
        };
        assert_eq!(
            max.validate(ProviderProtocol::OpenAiCompatible, &support),
            Err(OptionError::EffortUnsupported)
        );
        assert_eq!(max.validate(ProviderProtocol::Anthropic, &support), Ok(()));
    }

    #[test]
    fn each_dialect_receives_its_own_fields() {
        let options = ModelOptions {
            fast_mode: true,
            reasoning_effort: Some(ReasoningEffort::High),
        };
        let mut anthropic = serde_json::json!({ "model": "m", "output_config": { "format": {} } });
        let headers = options.apply(ProviderProtocol::Anthropic, &mut anthropic);
        assert_eq!(anthropic["speed"], "fast");
        assert_eq!(anthropic["output_config"]["effort"], "high");
        assert!(anthropic["output_config"]["format"].is_object());
        assert_eq!(headers, [("anthropic-beta", "fast-mode-2026-02-01")]);
        let mut azure = serde_json::json!({ "model": "m" });
        assert!(
            options
                .apply(ProviderProtocol::AzureOpenAi, &mut azure)
                .is_empty()
        );
        assert_eq!(azure["service_tier"], "priority");
        assert_eq!(azure["reasoning_effort"], "high");
    }
}
