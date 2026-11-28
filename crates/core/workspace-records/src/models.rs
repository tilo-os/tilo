//! The person's model providers and their keys, kept where their workspace
//! runs, and the actions a page, a window or an agent asks of them: declared
//! here, once, as [`ModelsAction`].
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
//! Local models are set up where they run, in a computer's own Agent models
//! pane; the model chosen there is linked here as the workspace's
//! ([`LINK_THIS_COMPUTER`]), so a workspace keeps one choice of model.
//!
//! What this declaration publishes ([`surface`]) is written beside this crate as
//! `models-surface.json`: a tilolang view compiles against it, and a crate that does not depend
//! on this one calls the actions through `tilo_resource::bindings!` of that file.

use serde::{Deserialize, Serialize};
use tilo_actions::ActionAddress;
use tilo_actions::remote::RemoteActionTarget;
use tilo_actions::remote::{KEEP_SEALED, KEEP_USES, REMOVE_SEALED};
use tilo_resource::Effect as _;

/// What each model provider action writes in the workspace's vault, by its id, as the host
/// answering it takes it: a key typed in is kept, a removed key goes (as does the one a vault
/// item's key replaces), and a provider asked with a key the vault keeps writes down that use, a
/// key just kept included. Signing in on the provider's own page keeps the key or grant it left.
/// An action not named writes nothing there.
pub const MODEL_VAULT_STEPS: [(&str, &[&str]); 11] = {
    const KEEPS: &[&str] = &[KEEP_SEALED, REMOVE_SEALED, KEEP_USES];
    [
        ("begin-sign-in", &[KEEP_SEALED]),
        ("add-provider", KEEPS),
        ("add-preset", KEEPS),
        ("set-key", KEEPS),
        ("set-key-from-item", &[KEEP_USES, REMOVE_SEALED]),
        ("remove-key", &[REMOVE_SEALED]),
        ("remove-provider", &[REMOVE_SEALED]),
        ("refresh-models", &[KEEP_USES]),
        ("search-hub-models", &[KEEP_USES]),
        ("show-hub-model", &[KEEP_USES]),
        ("install-hub-model", &[KEEP_USES]),
    ]
};

/// The provider that answers the person's model providers under.
pub const PROVIDER: &str = <ModelsAction as tilo_resource::Effect>::OPERATION;
/// The resource the providers are actions of.
pub const RESOURCE: &str = "models";

/// The model providers' actions, each with the parameters its argument carries. A key typed in
/// travels in [`ModelsAction::SetKey`] and is never answered back.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.models")]
pub enum ModelsAction {
    /// List model providers
    #[effect(kind = "read")]
    List,
    /// Add a model provider
    #[effect(kind = "undoable")]
    AddProvider {
        #[param("The name the person gives it.")]
        name: String,
        #[param("How it is spoken to: `open-ai-compatible` or `anthropic`.")]
        protocol: Protocol,
        #[param("The base URL every request goes to.")]
        endpoint: String,
        #[param("Headers sent with every request beside the key, each with `name` and `value`.")]
        headers: Option<Vec<RequestHeader>>,
    },
    /// Keep a provider's key
    #[effect(kind = "undoable")]
    SetKey {
        #[param("The provider's id, as the providers list names it.")]
        provider: String,
        #[param(
            secret,
            "The API key: sealed in the workspace's vault, never answered back."
        )]
        key: String,
    },
    /// Keep a provider's key from the vault
    ///
    /// Keeps the key a vault item holds as a provider's API key, answering the provider as a
    /// [`ProviderView`]. The key is named by reference and the host resolves it, only when the
    /// item belongs to the provider's endpoint. It carries no secret, so unlike `set-key` an
    /// agent acting for the person may ask it.
    #[effect(kind = "undoable")]
    SetKeyFromItem {
        #[param("The provider's id, as the providers list names it.")]
        provider: String,
        #[param(
            "The vault item that keeps the key, by its reference.",
            resource = "vault.item"
        )]
        key: String,
        #[param("Why whoever asks needs it, in a sentence its person reads.")]
        reason: Option<String>,
    },
    /// Remove a provider's key
    #[effect(kind = "undoable")]
    RemoveKey {
        #[param("The provider's id, as the providers list names it.")]
        provider: String,
    },
    /// Remove a model provider
    #[effect(kind = "undoable")]
    RemoveProvider {
        #[param("The provider's id, as the providers list names it.")]
        provider: String,
    },
    /// Add a ready-made provider
    #[effect(kind = "undoable")]
    AddPreset {
        #[param(
            "The ready-made provider's name: `anthropic`, `gemini`, `openai` or `openrouter`."
        )]
        preset: String,
    },
    /// Offer a provider to agents, or stop
    #[effect(kind = "undoable")]
    SetEnabled {
        #[param("The provider's id, as the providers list names it.")]
        provider: String,
        #[param("Whether its models are offered to the workspace's agents.")]
        enabled: bool,
    },
    /// Ask a provider for its models
    #[effect(kind = "undoable")]
    RefreshModels {
        #[param("The provider's id, as the providers list names it.")]
        provider: String,
    },
    /// Add a model a provider lists nowhere
    #[effect(kind = "undoable")]
    AddModel {
        #[param("The provider's id.")]
        provider: String,
        #[param("The model's id, as the provider's API names it.")]
        model: String,
    },
    /// Choose the model work runs on
    #[effect(kind = "undoable")]
    SetDefault {
        #[param("The provider's id.")]
        provider: String,
        #[param("The model's id.")]
        model: String,
    },
    /// Choose the model one kind of work runs on
    #[effect(kind = "undoable")]
    SetAssignment {
        #[param(
            "The kind of work: `one_shot`, `task`, `subtask`, `request`, `classifier` or `dictation`."
        )]
        kind: String,
        #[param("The provider's id; both left out, the kind follows the default again.")]
        provider: Option<String>,
        #[param("The model's id.")]
        model: Option<String>,
    },
    /// Choose one model for several kinds of work
    #[effect(kind = "undoable")]
    SetAssignments {
        #[param(
            "The kinds of work chosen together; both provider and model left out, they follow the default again."
        )]
        kinds: Vec<String>,
        #[param("The provider's id.")]
        provider: Option<String>,
        #[param("The model's id.")]
        model: Option<String>,
    },
    /// Choose how hard a provider's models think
    #[effect(kind = "undoable")]
    SetReasoning {
        #[param("The provider's id.")]
        provider: String,
        #[param(
            "How hard its models think: `none`, `minimal`, `low`, `medium`, `high` or `xhigh`; left out, the provider's own default applies again."
        )]
        effort: Option<String>,
    },
    /// Begin signing in to a provider
    ///
    /// Signs in with the person's `OpenAI` account, the way `OpenAI`'s own Codex signs in where
    /// it has no browser: the server asks `OpenAI` for a one-time code, and the person enters it
    /// on `OpenAI`'s page ([`SignInStarted`]). The server waits for `OpenAI` to grant it, keeps
    /// the grant in the person's vault and refreshes it there; no window ever holds a token.
    /// Only the person themselves may sign in.
    #[effect(kind = "undoable", returns = SignInStarted)]
    BeginSignIn {
        #[param("What the account is used for: `subscription` when left out, or `api-usage`.")]
        using: Option<AccountUse>,
    },
    /// Wait for a provider sign-in
    #[effect(kind = "read", returns = SignInState)]
    WaitSignIn {
        #[param("The sign-in, as beginning one named it.", name = "signIn")]
        sign_in: String,
    },
    /// Stop waiting for a provider sign-in
    #[effect(kind = "undoable")]
    CancelSignIn {
        #[param("The sign-in, as beginning one named it.", name = "signIn")]
        sign_in: String,
    },
    /// Search Hugging Face for models this computer runs
    #[effect(kind = "read")]
    SearchHubModels {
        #[param("What to look for, as Hugging Face searches it.")]
        query: String,
        #[param("`gguf` or `safetensors`; `gguf` when none is named.")]
        format: Option<String>,
    },
    /// Describe a Hugging Face model this computer runs
    #[effect(kind = "read")]
    ShowHubModel {
        #[param("The model's id or page on Hugging Face.")]
        model: String,
    },
    /// Download a Hugging Face model to this computer
    #[effect(kind = "undoable")]
    InstallHubModel {
        #[param("The model's repository id.")]
        repo: String,
        #[param("The commit to download.")]
        revision: String,
        #[param("The file variant to download.")]
        variant: String,
        #[param("Whether the model's licence is accepted.", name = "acceptLicense")]
        accept_license: Option<bool>,
    },
    /// Run the workspace on this computer's model
    ///
    /// Links the model this computer's Agent models pane chose as the workspace's: the providers
    /// it asks without a key (a model it runs itself, a server on its network) join the
    /// workspace's, and the workspace's work runs on that choice. Only a computer of the
    /// person's answers it, from its own pane; answers the providers as a [`ModelProviders`].
    #[effect(kind = "undoable")]
    LinkThisComputer,
    /// Ask the workspace's model
    ///
    /// One turn, as the model the person chose for a request (else their default) answers it,
    /// with the key their vault keeps. The argument is an `agent_provider_config` model
    /// request; the answer is a `Result` of its response and its error as serde writes one
    /// (`{"Ok": ...}` or `{"Err": ...}`), so a refusal keeps the kind a window decides by (no
    /// provider yet, no key) and the words the person reads.
    #[effect(kind = "read")]
    Ask,
    /// List the models this computer runs
    ///
    /// Asks the computer's model manager what it has installed, what it offers for this
    /// machine, and how far each setup has got. A computer without a manager answers
    /// [`LocalModels::available`] false and why.
    #[effect(kind = "read", returns = LocalModels)]
    LocalModels,
    /// Download a model the manager offers
    #[effect(kind = "undoable")]
    InstallLocalModel {
        #[param("The model's id, as the manager names it.")]
        model: String,
        #[param("The signed revision to download.")]
        revision: String,
        #[param("Whether the model's licence is accepted.", name = "acceptLicense")]
        accept_license: Option<bool>,
    },
    /// Start serving an installed model
    #[effect(kind = "undoable")]
    StartLocalModel {
        #[param("The model's id, as the manager names it.")]
        model: String,
    },
    /// Stop serving a model
    #[effect(kind = "undoable")]
    StopLocalModel {
        #[param("The model's id, as the manager names it.")]
        model: String,
    },
    /// Remove an installed model and its files
    #[effect(kind = "undoable")]
    RemoveLocalModel {
        #[param("The model's id, as the manager names it.")]
        model: String,
    },
    /// Stage the accelerator pack the recommended model needs
    #[effect(kind = "undoable")]
    StageAccelerator,
    /// Offer this computer's running model to the workspace
    ///
    /// Makes the model a workspace provider at the loopback address this computer's manager
    /// serves it at, and the model work runs on. The model must be installed and running, and
    /// the workspace must run on this computer: a server elsewhere reaches no loopback of the
    /// person's. Answers the providers as a [`ModelProviders`].
    #[effect(kind = "undoable", returns = ModelProviders)]
    UseLocalModel {
        #[param("The model's id, as the manager names it.")]
        model: String,
    },
}

/// Each action's id, as its declaration in [`ModelsAction`] names it.
macro_rules! ids {
    ($($id:ident),* $(,)?) => {
        $(
            #[doc = concat!("The id of [`ModelsAction::", stringify!($id), "`].")]
            pub const $id: &str = ModelsAction::$id.id();
        )*
    };
}

ids! {
    LIST, ADD_PROVIDER, SET_KEY, SET_KEY_FROM_ITEM, REMOVE_KEY, REMOVE_PROVIDER, ADD_PRESET,
    SET_ENABLED, REFRESH_MODELS, ADD_MODEL, SET_DEFAULT, SET_ASSIGNMENT, SET_ASSIGNMENTS,
    SET_REASONING, BEGIN_SIGN_IN, WAIT_SIGN_IN, CANCEL_SIGN_IN, SEARCH_HUB_MODELS,
    SHOW_HUB_MODEL, INSTALL_HUB_MODEL, LINK_THIS_COMPUTER, ASK, LOCAL_MODELS,
    INSTALL_LOCAL_MODEL, START_LOCAL_MODEL, STOP_LOCAL_MODEL, REMOVE_LOCAL_MODEL,
    STAGE_ACCELERATOR, USE_LOCAL_MODEL
}

/// The declaration of the action `action`, when it is one of [`ModelsAction`].
#[must_use]
pub fn declaration(action: &str) -> Option<tilo_resource::EffectDecl> {
    ModelsAction::declarations()
        .into_iter()
        .find(|decl| decl.id == action)
}

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// The workspace's model providers as a view of another language reads and changes them
/// (`models.models`): the reading's fields, the records it holds and the actions a view calls.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<ModelProviders, ModelsAction>(
        RESOURCE,
        "Model providers",
    )
}

/// How a provider is spoken to over HTTP.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// `/v1/chat/completions` and related endpoints.
    #[default]
    OpenAiCompatible,
    /// Anthropic's `/v1/messages` API.
    Anthropic,
}

/// One model a provider offers.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfferedModel {
    pub id: String,
    pub label: String,
}

/// One provider, as [`LIST`] and the changing actions answer it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
    /// Whether agents may use it. A provider the person stopped offering keeps
    /// its key and its models, and no kind of work is assigned to it.
    #[serde(default)]
    pub enabled: bool,
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
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestHeader {
    pub name: String,
    pub value: String,
}

/// The providers the workspace's host keeps, and what runs where.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
    /// The providers a person can add without signing in: the catalogue's,
    /// where a workspace has none of them yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<ProviderPreset>,
    /// The models the computer hosting this workspace runs, as its model manager last answered
    /// [`LOCAL_MODELS`]; absent until it was asked. Host state, so every member reads the same
    /// computer's models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalModels>,
}

/// A provider the catalogue knows, which a person may add by its name
/// ([`ADD_PRESET`]).
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPreset {
    /// The name [`ADD_PRESET`] takes.
    pub preset: String,
    /// What people call it.
    pub name: String,
    /// What it is, in one line.
    pub description: String,
    pub protocol: Protocol,
}

/// The model one kind of work runs on.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetKeyFromItem {
    pub provider: String,
    pub key: crate::vault::ItemRef,
    /// Why whoever asks needs it, in a sentence its person reads when they
    /// are asked first; empty when nothing was said.
    pub reason: String,
}

impl SetKeyFromItem {
    /// The key [`SET_KEY_FROM_ITEM`] keeps with `arguments`, read as its declaration names them;
    /// `None` where they do not read, or its key names no vault item.
    #[must_use]
    pub fn asked(arguments: &serde_json::Map<String, serde_json::Value>) -> Option<Self> {
        let Ok(ModelsAction::SetKeyFromItem {
            provider,
            key,
            reason,
        }) = ModelsAction::decode(SET_KEY_FROM_ITEM, arguments)
        else {
            return None;
        };
        Some(Self {
            provider,
            key: tilo_actions::remote::ItemRef::parse(&key)?,
            reason: reason.unwrap_or_default(),
        })
    }
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

/// What a person's `OpenAI` account is used for once they signed in.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum AccountUse {
    /// Their `ChatGPT` plan: models answer on the subscription they pay for.
    #[default]
    Subscription,
    /// Their `OpenAI` API organisation: models answer on API usage, billed per
    /// token. The server has `OpenAI` issue an API key for the account.
    ApiUsage,
}

/// A sign-in begun: what the person enters, and where.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInStarted {
    /// What [`WAIT_SIGN_IN`] and [`CANCEL_SIGN_IN`] name it by.
    pub sign_in: String,
    /// The one-time code the person enters on `page`.
    pub code: String,
    /// `OpenAI`'s page the code is entered on.
    pub page: String,
}

/// How a sign-in goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
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

/// A model this computer's model manager runs, or offers to run.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalModel {
    pub id: String,
    pub name: String,
    pub description: String,
    pub revision: String,
    /// The bytes to download, and the bytes it takes once installed.
    pub download_bytes: i64,
    pub installed_bytes: i64,
    pub vision: bool,
    /// It runs inside the apps that use it: it is installed, never started.
    pub in_process: bool,
    pub segmentation: bool,
    pub license: String,
    pub license_url: String,
    /// How far setting it up has got.
    pub phase: LocalPhase,
}

/// Where setting a local model up has got.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalPhase {
    /// `downloading`, `installing`, `ready` or `error`.
    pub state: String,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    /// How far it has got, 0 to 100; 0 when the phase says no.
    pub percent: i64,
    /// Whether it is serving now.
    pub running: bool,
    /// The loopback address agents ask it at, while it serves; empty otherwise.
    pub endpoint: String,
    /// Why setting it up failed, or what an install is doing.
    pub message: String,
}

/// This computer's fit for a local model: its memory, accelerator and free storage.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalHardware {
    /// `excellent`, `good`, `tight` or `unsupported`.
    pub fit: String,
    pub memory_gib: i64,
    pub accelerator: String,
    /// 0 when the accelerator has no memory of its own.
    pub accelerator_memory_gib: i64,
    /// 0 when the computer does not say.
    pub free_storage_gib: i64,
    pub reason: String,
}

/// The models this computer runs, and what it offers to run ([`LOCAL_MODELS`]).
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalModels {
    /// Whether this computer's model manager answered.
    pub available: bool,
    /// Why it did not answer; absent when it did.
    pub problem: Option<String>,
    pub hardware: LocalHardware,
    /// The model the manager picks for this computer, where it names one.
    pub recommendation: Option<LocalModel>,
    /// The models installed and served by a runtime here.
    pub models: Vec<LocalModel>,
    /// The models that run inside the apps that use them, installed or not.
    pub in_process: Vec<LocalModel>,
    /// The accelerator pack's state (`available`, `queued`, `staging`, `restart-required`,
    /// `active` or `error`), empty when there is none.
    pub accelerator: String,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// The file a tilolang view compiles the workspace's model providers against, and a crate
    /// that does not depend on this one binds its calls to, is what this crate declares.
    /// `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_models_surface_file_is_what_this_crate_publishes() {
        let text =
            tilo_surface_compile::canonical(&serde_json::json!({ "resources": [surface()] }));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str()),
            "crates/core/workspace-records/models-surface.json is not what this crate \
             publishes; regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }

    /// What the action registry knows of the providers without this crate names them as they
    /// are declared here: the provider they answer under, and only actions they declare as the
    /// ones that write in the vault.
    #[test]
    fn the_registry_names_the_declared_actions() {
        assert_eq!(PROVIDER, tilo_actions::remote::MODELS_PROVIDER);
        for (action, _) in crate::models::MODEL_VAULT_STEPS {
            assert!(
                declaration(action).is_some(),
                "{action} is declared nowhere"
            );
        }
    }

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
            enabled: true,
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
        let asked = |arguments: serde_json::Value| {
            ModelsAction::decode(SET_REASONING, arguments.as_object().unwrap()).unwrap()
        };
        let reasoning = |effort: Option<&str>| ModelsAction::SetReasoning {
            provider: "gemini".into(),
            effort: effort.map(Into::into),
        };
        let choosing = asked(serde_json::json!({"provider": "gemini", "effort": "low"}));
        assert_eq!(choosing, reasoning(Some("low")));
        let leaving = asked(serde_json::json!({"provider": "gemini"}));
        assert_eq!(leaving, reasoning(None));

        let plain = serde_json::to_value(ProviderView::default()).unwrap();
        assert!(plain.get("reasoning").is_none(), "{plain}");
        let chosen = serde_json::to_value(ProviderView {
            reasoning: Some("low".into()),
            ..ProviderView::default()
        })
        .unwrap();
        assert_eq!(chosen["reasoning"], "low");
    }
}
