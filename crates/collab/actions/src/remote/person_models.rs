//! The person's model providers and their keys, kept where their workspace
//! runs.
//!
//! A page has no vault of its own and no provider key of its own. The host
//! that runs the workspace keeps the providers the person set up, their API
//! keys sealed in the person's vault, and the model each kind of work
//! runs on; a page reads them and changes them here. A key travels one way
//! only: [`SET_KEY`] carries it in, [`ProviderView::configured`] says whether
//! one is kept, and no reading carries it back.
//!
//! Local models are not here: they run on a device, which is where the pane
//! that manages them lives.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider the host answers the person's model providers under.
pub const PROVIDER: &str = "tilo.models";
/// The resource the providers are actions of.
pub const RESOURCE: &str = "models";
/// The providers and their models, as a [`ModelProviders`].
pub const LIST: &str = "list";
/// Add a provider ([`AddProvider`]) and answer it as a [`ProviderView`].
pub const ADD_PROVIDER: &str = "add-provider";
/// Keep a provider's API key in the person's vault ([`SetKey`]), answering
/// the provider as a [`ProviderView`].
pub const SET_KEY: &str = "set-key";
/// Remove a provider's key from the person's vault (`provider`),
/// answering the provider as a [`ProviderView`].
pub const REMOVE_KEY: &str = "remove-key";
/// Remove a provider and its key (`provider`), answering the providers that
/// remain as a [`ModelProviders`].
pub const REMOVE_PROVIDER: &str = "remove-provider";
/// Choose the model one kind of work runs on ([`SetAssignment`]), answering
/// the providers as a [`ModelProviders`].
pub const SET_ASSIGNMENT: &str = "set-assignment";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// How a provider is spoken to over HTTP.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// `/v1/chat/completions` and related endpoints.
    #[default]
    OpenAiCompatible,
    /// Anthropic's `/v1/messages` API.
    Anthropic,
}

/// One model a provider offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfferedModel {
    pub id: String,
    pub label: String,
}

/// One provider, as [`LIST`] and the changing actions answer it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderView {
    /// The provider's id, as every other action names it.
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub endpoint: String,
    /// Whether the person's vault keeps an API key for it.
    pub configured: bool,
    pub models: Vec<OfferedModel>,
    /// The model this provider's agent runs when nothing else is chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

/// The providers the workspace's host keeps, and what runs where.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelProviders {
    pub providers: Vec<ProviderView>,
    /// The provider used when no kind of work names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    /// The model each kind of work runs on, by kind (`one_shot`, `task`,
    /// `subtask`, `request`, `classifier`, `dictation`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignments: Vec<Assignment>,
}

/// The model one kind of work runs on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Assignment {
    pub kind: String,
    pub provider: String,
    pub model: String,
}

/// A provider to add.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddProvider {
    /// The name the person gives it, shown in the list.
    pub name: String,
    pub protocol: Protocol,
    /// The base URL every request goes to.
    pub endpoint: String,
}

/// A provider's API key to keep. The key travels in and is never read back.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetKey {
    pub provider: String,
    pub key: String,
}

impl std::fmt::Debug for SetKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetKey")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// The model one kind of work runs on, or with neither provider nor model,
/// that kind follows the default again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetAssignment {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// A key never reaches a log, and no reading has a field that could
    /// carry it: the view says only whether one is kept.
    #[test]
    fn nothing_read_back_carries_a_key() {
        let setting = SetKey {
            provider: "openai".into(),
            key: "sk-live-secret".into(),
        };
        let printed = format!("{setting:?}");
        assert!(!printed.contains("sk-live-secret"), "{printed}");

        let view = ProviderView {
            id: "openai".into(),
            name: "OpenAI".into(),
            protocol: Protocol::OpenAiCompatible,
            endpoint: "https://api.openai.com/v1".into(),
            configured: true,
            models: vec![OfferedModel {
                id: "gpt".into(),
                label: "GPT".into(),
            }],
            default_model: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("sk-live-secret"));
        assert!(json.contains("\"configured\":true"), "{json}");
    }

    /// A provider is chosen for one kind of work by name, and the kind is
    /// the one the model configuration stores.
    #[test]
    fn an_assignment_names_its_kind_and_model() {
        let choosing: SetAssignment = serde_json::from_value(serde_json::json!({
            "kind": "task",
            "provider": "openai",
            "model": "gpt",
        }))
        .unwrap();
        assert_eq!(choosing.kind, "task");
        assert_eq!(choosing.provider.as_deref(), Some("openai"));
        assert_eq!(choosing.model.as_deref(), Some("gpt"));
        let following: SetAssignment =
            serde_json::from_value(serde_json::json!({"kind": "task"})).unwrap();
        assert_eq!(following.provider, None);
    }
}
