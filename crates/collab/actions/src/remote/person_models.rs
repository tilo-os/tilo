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
//! The host asks a provider for its models too ([`REFRESH_MODELS`]), with the
//! key it keeps: a page never holds a key it could ask with. For the same
//! reason a page's windows ask the person's model here ([`ASK`]): the server
//! makes the call with the key from the vault, as it does for the person's
//! agents, and answers the turn.
//!
//! Local models are not here: they run on a device, which is where the pane
//! that manages them lives.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider that answers the person's model providers under.
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
/// Keep the key a vault item holds as a provider's API key
/// ([`SetKeyFromItem`]), answering the provider as a [`ProviderView`]. The key
/// is named by reference and the host resolves it, only when the item belongs
/// to the provider's endpoint. It carries no secret, so unlike [`SET_KEY`] an
/// agent acting for the person may ask it.
pub const SET_KEY_FROM_ITEM: &str = "set-key-from-item";
/// Remove a provider's key from the person's vault (`provider`),
/// answering the provider as a [`ProviderView`].
pub const REMOVE_KEY: &str = "remove-key";
/// Remove a provider and its key (`provider`), answering the providers that
/// remain as a [`ModelProviders`].
pub const REMOVE_PROVIDER: &str = "remove-provider";
/// Add a provider the catalogue knows ([`AddPreset`]): its name, protocol,
/// endpoint and where it lists its models are the catalogue's. Answers it as
/// a [`ProviderView`].
pub const ADD_PRESET: &str = "add-preset";
/// Ask a provider (`provider`) for the models it offers, with the key the
/// vault keeps for it, and keep that list: a [`ListedModels`].
pub const REFRESH_MODELS: &str = "refresh-models";
/// Add a model a provider offers and lists nowhere ([`AddModel`]), answering
/// the provider as a [`ProviderView`].
pub const ADD_MODEL: &str = "add-model";
/// Choose the provider and model work runs on when its kind names none
/// ([`SetDefault`]), answering the providers as a [`ModelProviders`].
pub const SET_DEFAULT: &str = "set-default";
/// Choose the model one kind of work runs on ([`SetAssignment`]), answering
/// the providers as a [`ModelProviders`].
pub const SET_ASSIGNMENT: &str = "set-assignment";
/// Choose one model for several kinds of work together
/// ([`SetAssignments`]), answering the providers as a [`ModelProviders`].
pub const SET_ASSIGNMENTS: &str = "set-assignments";
/// Choose how hard a provider's models think ([`SetReasoning`]), answering
/// the provider as a [`ProviderView`]. With no level the provider's own
/// default holds again, and it is told nothing.
pub const SET_REASONING: &str = "set-reasoning";

/// Ask the person's model for one turn, as the model they chose for a
/// request (else their default) answers it, with the key their vault keeps.
/// The argument is an `agent_provider_config::protocol::ModelRequest`; the
/// answer is a `Result<ModelResponse, ModelError>` as serde writes one
/// (`{"Ok": ...}` or `{"Err": ...}`), so a refusal keeps the kind a window
/// decides by (no provider yet, no key) and the words the person reads.
pub const ASK: &str = "ask";

/// Begin signing in with the person's `OpenAI` account ([`BeginSignIn`]),
/// the way `OpenAI`'s own Codex signs in where it has no browser: the server
/// asks `OpenAI` for a one-time code, and the person enters it on `OpenAI`'s
/// page ([`SignInStarted`]). The server waits for `OpenAI` to grant it, keeps
/// the grant in the person's vault and refreshes it there; no window ever
/// holds a token. Only the person themselves may sign in.
pub const BEGIN_SIGN_IN: &str = "begin-sign-in";
/// How a sign-in begun with [`BEGIN_SIGN_IN`] goes ([`WaitSignIn`]),
/// answered as a [`SignInState`] once it changed or after a while.
pub const WAIT_SIGN_IN: &str = "wait-sign-in";
/// Stop waiting for a sign-in begun with [`BEGIN_SIGN_IN`] ([`WaitSignIn`]).
pub const CANCEL_SIGN_IN: &str = "cancel-sign-in";

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
    /// Whether the person's vault keeps an API key for it, or the grant of
    /// the account it signs in with.
    pub configured: bool,
    /// Whether it is reached by signing in to an account ([`BEGIN_SIGN_IN`])
    /// rather than with an API key.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub signs_in: bool,
    pub models: Vec<OfferedModel>,
    /// The model this provider's agent runs when nothing else is chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// How hard this provider's models are asked to think, where the person
    /// chose a level ([`SET_REASONING`]). Absent, the provider is told
    /// nothing and its own default holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Headers sent with every request to it beside its key ([`AddProvider::headers`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<RequestHeader>,
}

/// A header a provider is sent with every request: a setting, never its key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestHeader {
    pub name: String,
    pub value: String,
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
    /// Headers sent with every request beside the key, which the provider
    /// asks of each call: the workspace or organisation a key with access to
    /// several bills (`anthropic-workspace-id`, `openai-organization`). The
    /// headers the key and the protocol travel in are refused.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<RequestHeader>,
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

/// A provider's API key to keep from the person's vault. Nothing here is a
/// secret: `key` names the vault item, which must be a key that belongs to
/// the provider's endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetKeyFromItem {
    pub provider: String,
    pub key: super::person_vault::ItemRef,
    /// Why whoever asks needs it, in a sentence its person reads when they
    /// are asked first; empty when nothing was said.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
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

/// A provider of the catalogue to add, by the catalogue's name for it
/// (`anthropic`, `gemini`, `openai`, `openrouter`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddPreset {
    pub preset: String,
}

/// A provider after its models were asked for ([`REFRESH_MODELS`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListedModels {
    /// The provider, with the models now kept for it: the list it answered,
    /// or the one kept before when it answered none.
    pub provider: ProviderView,
    /// Why the provider's list could not be read; absent when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<ListingProblem>,
}

/// Why a provider's list of models could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListingProblem {
    /// No key is kept for the provider, so it was not asked.
    NoKey,
    /// The provider did not accept the key.
    KeyRefused,
    /// The provider has no list of models: its models are named by hand
    /// ([`ADD_MODEL`]).
    NoList,
    /// The provider did not answer.
    Unreachable,
    /// The host does not call this address: it is not an https address on
    /// the internet, and the host reaches no other.
    NotAllowed,
    /// The provider answered something that is no list of models.
    Unreadable,
}

/// A model to add to a provider that lists none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddModel {
    pub provider: String,
    /// The model's id, as the provider's API names it.
    pub model: String,
}

/// The provider and model work runs on when its kind names none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetDefault {
    pub provider: String,
    pub model: String,
}

/// One model for several kinds of work, chosen together so they never
/// differ by half a change: a person's main thread answers as `request` and
/// works as `task`. With neither provider nor model, those kinds follow the
/// default again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetAssignments {
    pub kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// How hard a provider's models think: `none`, `minimal`, `low`, `medium`,
/// `high` or `xhigh`, of which a provider takes the ones its models know.
/// With no level, the provider's own default holds again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetReasoning {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// What a person's `OpenAI` account is used for once they signed in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountUse {
    /// Their `ChatGPT` plan: models answer on the subscription they pay for.
    #[default]
    Subscription,
    /// Their `OpenAI` API organisation: models answer on API usage, billed per
    /// token. The server has `OpenAI` issue an API key for the account.
    ApiUsage,
}

/// A sign-in to begin.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BeginSignIn {
    #[serde(default)]
    pub using: AccountUse,
}

/// A sign-in begun: what the person enters, and where.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInStarted {
    /// What [`WAIT_SIGN_IN`] and [`CANCEL_SIGN_IN`] name it by.
    pub sign_in: String,
    /// The one-time code the person enters on `page`.
    pub code: String,
    /// `OpenAI`'s page the code is entered on.
    pub page: String,
}

/// A sign-in to wait for, or to stop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitSignIn {
    pub sign_in: String,
}

/// How a sign-in goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum SignInState {
    /// The person has not entered the code yet.
    Waiting,
    /// Signed in: the provider that answers on the account now, its models
    /// listed.
    Done { provider: ProviderView },
    /// It ended without a grant, for the reason a person reads.
    Failed { reason: String },
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
            reasoning: None,
            signs_in: false,
            headers: Vec::new(),
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

    /// A list that could not be read says why in a word a page can put its
    /// own sentence to, and a list that was read says nothing.
    #[test]
    fn a_listing_names_its_problem_or_none() {
        let read = ListedModels::default();
        let json = serde_json::to_value(&read).unwrap();
        assert!(json.get("problem").is_none(), "{json}");
        let refused = ListedModels {
            problem: Some(ListingProblem::KeyRefused),
            ..ListedModels::default()
        };
        let json = serde_json::to_value(&refused).unwrap();
        assert_eq!(json["problem"], serde_json::json!("key-refused"));
        assert_eq!(
            serde_json::from_value::<ListedModels>(json).unwrap(),
            refused
        );
    }

    /// A thinking level is chosen for a provider by name and taken back by
    /// naming none; a provider's view says a level only where one is chosen.
    #[test]
    fn a_thinking_level_is_chosen_or_left_to_the_provider() {
        let choosing: SetReasoning =
            serde_json::from_value(serde_json::json!({"provider": "gemini", "effort": "low"}))
                .unwrap();
        assert_eq!(choosing.effort.as_deref(), Some("low"));
        let leaving: SetReasoning =
            serde_json::from_value(serde_json::json!({"provider": "gemini"})).unwrap();
        assert_eq!(leaving.effort, None);

        let plain = serde_json::to_value(ProviderView::default()).unwrap();
        assert!(plain.get("reasoning").is_none(), "{plain}");
        let chosen = serde_json::to_value(ProviderView {
            reasoning: Some("low".into()),
            ..ProviderView::default()
        })
        .unwrap();
        assert_eq!(chosen["reasoning"], "low");
    }

    /// The main thread's two kinds are chosen in one request.
    #[test]
    fn several_kinds_take_one_model_together() {
        let choosing: SetAssignments = serde_json::from_value(serde_json::json!({
            "kinds": ["request", "task"],
            "provider": "gemini",
            "model": "gemini-pro",
        }))
        .unwrap();
        assert_eq!(choosing.kinds, ["request", "task"]);
        assert_eq!(choosing.model.as_deref(), Some("gemini-pro"));
    }
}
