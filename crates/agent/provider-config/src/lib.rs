//! Provider-neutral configuration for Tilo agents.
//!
//! This crate stores *descriptions* of model providers, never a
//! bearer token. A profile names its credential ([`Credential`]): an API key
//! in the system vault or a signed-in online account, while the process that
//! owns the vault resolves it at call time. Keeping this boundary here makes
//! the same profile safe to render in Settings, sync as personal intent, or
//! hand to a sandboxed agent.
//!
//! ## Where each record lives
//!
//! The `agent_providers` settings section holds three leaves with their own
//! scopes (see `settings_store::continuity`):
//!
//! - `profiles` — **personal**. Hosted, Azure and private-network endpoints
//!   the person configured. They follow the person to each of their devices;
//!   the keys they point at do not (the vault is per device), so a second
//!   device shows the provider as needing its key.
//! - `this_device` — **device-local**. Servers the person runs on this
//!   computer themselves (Ollama, LM Studio). Tilo does not isolate them, so
//!   for privacy they count as off the device.
//! - `assignments` — **personal**. Which model serves which task and agent
//!   ([`assign::ModelAssignments`]). Assignments that name a local model name
//!   it by catalogue id, so they resolve on whichever device runs it.
//!
//! The model manager's own record ([`LocalRuntime`]: this machine's hardware
//! facts and the profile of the models it supervises) is not a setting. The
//! manager owns it and rewrites it on every start, stop, failure and
//! install, at [`LocalRuntime::default_path`]; everything else only reads it.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod assign;
pub mod local;
pub mod manager;
pub mod options;
pub mod tasks;

pub use options::{ModelOptionSupport, ModelOptions, OptionError, ReasoningEffort};

/// The `settings.json` section owned by this crate.
pub const SETTINGS_SECTION: &str = "agent_providers";
/// Personal leaf: the person's configured provider profiles.
pub const PROFILES_KEY: &str = "profiles";
/// Device-local leaf: servers the person runs on this computer.
pub const THIS_DEVICE_KEY: &str = "this_device";
/// Personal leaf: [`assign::ModelAssignments`].
pub const ASSIGNMENTS_KEY: &str = "assignments";

/// The request dialect spoken by a provider endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    /// `/v1/chat/completions` and related OpenAI-compatible endpoints.
    #[default]
    OpenAiCompatible,
    /// Anthropic's `/v1/messages` API.
    Anthropic,
    /// Azure `OpenAI` v1 API: `OpenAI`-compatible requests against a resource
    /// endpoint, where each model is a named deployment and the credential is
    /// an `api-key` header or a Microsoft Entra ID bearer token.
    AzureOpenAi,
}

impl ProviderProtocol {
    pub const ALL: &'static [Self] = &[Self::OpenAiCompatible, Self::Anthropic, Self::AzureOpenAi];

    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAI-compatible",
            Self::Anthropic => "Anthropic",
            Self::AzureOpenAi => "Azure OpenAI",
        }
    }

    /// Stable slug used in typed action payloads.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai-compatible",
            Self::Anthropic => "anthropic",
            Self::AzureOpenAi => "azure-openai",
        }
    }

    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|protocol| protocol.slug() == slug)
    }

    /// Whether the protocol accepts an online-account sign-in instead of a key.
    pub const fn supports_account_sign_in(self) -> bool {
        matches!(self, Self::AzureOpenAi)
    }
}

/// Where an endpoint runs. This is an explicit trust cue in the UI: a local
/// vLLM server must remain distinct from a hosted API even though it shares the
/// `OpenAI` wire protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointLocation {
    /// A provider-operated internet endpoint.
    #[default]
    Hosted,
    /// A loopback endpoint on this Tilo device, e.g. vLLM or Ollama.
    ThisDevice,
    /// An endpoint on a LAN, VPN, or a user-managed server.
    PrivateNetwork,
}

impl EndpointLocation {
    pub const ALL: &'static [Self] = &[Self::Hosted, Self::ThisDevice, Self::PrivateNetwork];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Hosted => "Hosted provider",
            Self::ThisDevice => "This device (local)",
            Self::PrivateNetwork => "Private network",
        }
    }

    pub const fn needs_tls(self) -> bool {
        matches!(self, Self::Hosted)
    }
}

/// A stable pointer to a secret held outside this configuration document.
///
/// `locator` is an opaque non-secret item name such as
/// `agent-provider/01J.../api-key`. It is not an environment
/// variable name: the desktop broker can apply per-agent access policy before
/// handing the secret to an outbound request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretReference {
    /// The broker/vault namespace, usually `tilo.secret-service`.
    pub vault: String,
    /// Opaque item locator within `vault`; never the secret value.
    pub locator: String,
}

impl SecretReference {
    pub fn provider_api_key(profile_id: &str) -> Self {
        Self {
            vault: "tilo.secret-service".to_string(),
            locator: format!("agent-provider/{profile_id}/api-key"),
        }
    }

    pub fn is_valid(&self) -> bool {
        !self.vault.trim().is_empty()
            && !self.locator.trim().is_empty()
            && !self.locator.contains('\n')
            && !self.locator.contains('\r')
    }
}

/// How the gateway authenticates to a provider. Neither variant holds a
/// secret: both name where the gateway finds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// An API key the person entered, sealed in the vault.
    ApiKey { reference: SecretReference },
    /// An online account (`accounts::AccountStore`) signed in through
    /// OAuth; account custody holds its grant and hands the gateway a
    /// current access token for it.
    Account { account_id: String },
}

impl Credential {
    fn is_valid(&self) -> bool {
        match self {
            Self::ApiKey { reference } => reference.is_valid(),
            Self::Account { account_id } => {
                !account_id.trim().is_empty() && !account_id.chars().any(char::is_control)
            }
        }
    }
}

/// A fixed gateway protocol a model declares it serves. For locally managed
/// models the set is copied from the signed catalogue; for hosted profiles it
/// remains an explicit user or provider claim. It is never inferred from a
/// model's name, and no capability implies another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    /// Accepts bounded image input: Photos' captions, labels and OCR.
    Vision,
    /// The dedicated local Photos face-clustering protocol. Generic image
    /// description never enables biometrics.
    FaceRecognition,
    /// The feedback-review protocol: redaction proposals and step titles over
    /// the text of a recording.
    TraceReview,
}

impl ModelCapability {
    pub const ALL: [Self; 3] = [Self::Vision, Self::FaceRecognition, Self::TraceReview];

    /// Fluent key of the capability's name.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Vision => "ai-capability-vision",
            Self::FaceRecognition => "ai-capability-face-recognition",
            Self::TraceReview => "ai-capability-trace-review",
        }
    }
}

/// One model advertised by a profile. A model list is metadata, not a
/// capability grant: individual agent tasks still require their own action
/// permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelDescriptor {
    /// Provider-native model id sent over the wire. For Azure `OpenAI` this is
    /// the deployment name.
    pub id: String,
    /// User-facing label. Defaults to `id` for imported/minimal profiles.
    pub label: String,
    /// Best known input-context size. `None` means unknown, not unlimited.
    pub context_window: Option<u32>,
    /// Immutable release revision when this model comes from the signed local
    /// model catalogue. Hosted providers often cannot supply one, so absence
    /// means "not attested", never "latest".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The fixed gateway protocols this model declares.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub capabilities: BTreeSet<ModelCapability>,
    pub tool_calling: bool,
    /// The typed request options this model accepts.
    pub options: ModelOptionSupport,
}

impl Default for ModelDescriptor {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            context_window: None,
            revision: None,
            capabilities: BTreeSet::new(),
            tool_calling: true,
            options: ModelOptionSupport::default(),
        }
    }
}

impl ModelDescriptor {
    #[must_use]
    pub fn declares(&self, capability: ModelCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            label: id.clone(),
            id,
            ..Self::default()
        }
    }

    pub fn display_label(&self) -> &str {
        if self.label.trim().is_empty() {
            &self.id
        } else {
            &self.label
        }
    }
}

/// Who runs an endpoint, and so what it can reach.
///
/// Only Tilo's model manager makes an endpoint [`Self::Supervised`]: it runs
/// a pinned, signed runtime in a container without a network and serves it
/// through a private Unix socket. Settings records that from the manager's
/// ready state; no form offers it. Every endpoint a person types in, even on
/// `localhost`, is [`Self::UserConfigured`], because it could forward what it
/// is sent anywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointCustody {
    #[default]
    UserConfigured,
    /// Reached only through `socket`; the runtime has no network egress.
    Supervised { socket: std::path::PathBuf },
}

/// The HTTP base a supervised runtime answers on, over its socket. The host
/// name is never resolved: the gateway connects to the socket itself.
pub const SUPERVISED_BASE_URL: &str = "http://localhost/v1";

/// A configured model-provider endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderProfile {
    pub id: String,
    pub label: String,
    pub protocol: ProviderProtocol,
    pub endpoint: String,
    pub location: EndpointLocation,
    /// Absent for an unauthenticated local endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<Credential>,
    pub enabled: bool,
    pub models: Vec<ModelDescriptor>,
    /// The default model id within `models`. Missing/unknown is represented
    /// by `None`, so callers prompt instead of silently choosing a model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub custody: EndpointCustody,
    /// How an OpenAI-dialect request (OpenAI-compatible or Azure `OpenAI`)
    /// names its output-token limit. The model gateway owns the outbound
    /// request and renames whichever limit the caller sent to this one.
    /// `None` sends the caller's name unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_limit_param: Option<TokenLimitParam>,
    /// The reasoning setting the gateway sends with every OpenAI-dialect
    /// request of this profile, whatever the caller asked for. The field it
    /// names carries the assignment's reasoning effort when the assignment
    /// sets one ([`ModelOptions`]), and this effort otherwise. `None` leaves
    /// the reasoning field to the assignment's options (the provider's
    /// default when they set none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningSetting>,
    /// Top-level request fields this provider rejects, removed by the gateway
    /// after it has shaped the request (e.g. `stream_options`,
    /// `parallel_tool_calls`). Each is a plain JSON key; `model`,
    /// `messages`, `tools` and `tool_choice` cannot be listed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub omit_params: Vec<String>,
}

/// Request fields a profile may not strip: without them there is no request.
pub const REQUIRED_PARAMS: &[&str] = &["model", "messages", "tools", "tool_choice"];

/// The output-token-limit field of an OpenAI-compatible provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenLimitParam {
    #[serde(rename = "max_tokens")]
    MaxTokens,
    #[serde(rename = "max_completion_tokens")]
    MaxCompletionTokens,
}

impl TokenLimitParam {
    pub fn key(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::MaxCompletionTokens => "max_completion_tokens",
        }
    }
}

/// Where a provider takes its reasoning effort.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningParam {
    /// `OpenAI` Chat Completions: top-level `"reasoning_effort": "<effort>"`.
    #[serde(rename = "reasoning_effort")]
    ReasoningEffort,
    /// `OpenRouter`'s unified object: `"reasoning": {"effort": "<effort>"}`.
    #[serde(rename = "reasoning.effort")]
    ReasoningObject,
    /// Send no reasoning field at all, and strip any the caller sent.
    #[serde(rename = "none")]
    Omit,
}

/// A profile's reasoning setting, applied by the gateway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningSetting {
    pub param: ReasoningParam,
    /// One of [`REASONING_EFFORTS`]; unused when `param` is `none`. Not every
    /// model accepts every value (GPT-5.4 rejects `minimal`, gpt-oss cannot
    /// disable reasoning so its lowest is `low`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effort: String,
}

/// The reasoning efforts a profile may name.
pub const REASONING_EFFORTS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh"];

impl Default for ProviderProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            protocol: ProviderProtocol::default(),
            endpoint: String::new(),
            location: EndpointLocation::default(),
            credential: None,
            enabled: true,
            models: Vec::new(),
            default_model: None,
            custody: EndpointCustody::UserConfigured,
            token_limit_param: None,
            reasoning: None,
            omit_params: Vec::new(),
        }
    }
}

impl ProviderProfile {
    /// A blank profile of `protocol` at `location`, with an allocated id and,
    /// for anything off this device, a vault reference for its key. The
    /// caller can open a system credential prompt for that reference; no UI
    /// needs to retain the typed API key in its widget state or CRDT.
    #[must_use]
    pub fn new(protocol: ProviderProtocol, location: EndpointLocation) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let (label, endpoint) = match (protocol, location) {
            (ProviderProtocol::Anthropic, _) => ("Anthropic", "https://api.anthropic.com"),
            (ProviderProtocol::AzureOpenAi, _) => (
                "Azure OpenAI",
                "https://RESOURCE.openai.azure.com/openai/v1",
            ),
            (ProviderProtocol::OpenAiCompatible, EndpointLocation::Hosted) => {
                ("OpenAI-compatible provider", "https://api.openai.com/v1")
            }
            (ProviderProtocol::OpenAiCompatible, EndpointLocation::PrivateNetwork) => {
                ("Compatible server", "http://server.lan:8000/v1")
            }
            (ProviderProtocol::OpenAiCompatible, EndpointLocation::ThisDevice) => {
                ("Server on this computer", "http://127.0.0.1:11434/v1")
            }
        };
        let credential = (location != EndpointLocation::ThisDevice).then(|| Credential::ApiKey {
            reference: SecretReference::provider_api_key(&id),
        });
        Self {
            id,
            label: label.to_string(),
            protocol,
            endpoint: endpoint.to_string(),
            location,
            credential,
            enabled: true,
            models: Vec::new(),
            default_model: None,
            custody: EndpointCustody::UserConfigured,
            token_limit_param: None,
            reasoning: None,
            omit_params: Vec::new(),
        }
    }

    /// A hosted profile of `protocol`.
    #[must_use]
    pub fn hosted(protocol: ProviderProtocol) -> Self {
        Self::new(protocol, EndpointLocation::Hosted)
    }

    /// The profile of the models the model manager runs on this device,
    /// served through `socket` (`None` while no model runs). Unauthenticated:
    /// only the gateway reaches the socket.
    #[must_use]
    pub fn local_runtime(socket: Option<std::path::PathBuf>) -> Self {
        let running = socket.is_some();
        Self {
            id: LOCAL_PROFILE_ID.to_string(),
            label: "This device".to_string(),
            protocol: ProviderProtocol::OpenAiCompatible,
            endpoint: SUPERVISED_BASE_URL.to_string(),
            location: EndpointLocation::ThisDevice,
            credential: None,
            enabled: running,
            models: Vec::new(),
            default_model: None,
            custody: socket.map_or(EndpointCustody::UserConfigured, |socket| {
                EndpointCustody::Supervised { socket }
            }),
            token_limit_param: None,
            reasoning: None,
            omit_params: Vec::new(),
        }
    }

    pub fn display_label(&self) -> &str {
        if self.label.trim().is_empty() {
            &self.id
        } else {
            &self.label
        }
    }

    pub fn model(&self, id: &str) -> Option<&ModelDescriptor> {
        self.models.iter().find(|model| model.id == id)
    }

    /// Whether this is an OpenAI-compatible endpoint on this device's
    /// loopback interface: the only kind of profile the gateway's fixed local
    /// verbs (image analysis, face clustering, trace review) run on. Whether
    /// it is running is `enabled`, checked separately.
    #[must_use]
    pub fn is_device_local(&self) -> bool {
        self.location == EndpointLocation::ThisDevice
            && self.protocol == ProviderProtocol::OpenAiCompatible
            && match &self.custody {
                EndpointCustody::Supervised { socket } => socket.is_absolute(),
                EndpointCustody::UserConfigured => is_loopback_endpoint(&self.endpoint),
            }
    }

    /// Whether this is the model manager's profile of the models it
    /// supervises on this device, running or not.
    #[must_use]
    pub fn is_model_manager(&self) -> bool {
        self.id == LOCAL_PROFILE_ID
    }

    /// Whether this is a server the person runs on this computer
    /// themselves: on loopback, but not isolated by Tilo.
    #[must_use]
    pub fn is_user_local_server(&self) -> bool {
        !self.is_model_manager()
            && self.location == EndpointLocation::ThisDevice
            && self.custody == EndpointCustody::UserConfigured
    }

    /// Whether Tilo's model manager runs this endpoint, now, without network
    /// egress, so what it is sent cannot leave the device.
    #[must_use]
    pub fn is_supervised(&self) -> bool {
        self.enabled
            && self.is_model_manager()
            && self.is_device_local()
            && matches!(self.custody, EndpointCustody::Supervised { .. })
    }

    /// `model_id` of this profile when the profile is device-local and
    /// running, the model declares `capability`, and its revision is pinned.
    pub fn pinned_local_model(
        &self,
        model_id: &str,
        capability: ModelCapability,
    ) -> Option<&ModelDescriptor> {
        if !self.is_supervised() {
            return None;
        }
        self.models.iter().find(|model| {
            model.id == model_id
                && model.declares(capability)
                && model.revision.as_deref().is_some_and(is_pinned_revision)
        })
    }
}

/// The fixed id of this device's model-manager profile.
pub const LOCAL_PROFILE_ID: &str = "this-device";

/// Whether `endpoint` is an `http(s)` URL on a loopback host, without
/// credentials embedded in it.
#[must_use]
pub fn is_loopback_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    }
}

/// Whether `revision` is an immutable 40- or 64-digit lowercase hex digest.
#[must_use]
pub fn is_pinned_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64)
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// What the model manager publishes for this device: the hardware facts
/// presets and fit are derived from, and the loopback profile of the models
/// installed here. Device-local; never roams.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalRuntime {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardware: Option<local::HardwareSnapshot>,
    /// Present once any model is installed. Enabled only while a model runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProviderProfile>,
}

impl LocalRuntime {
    /// Where the model manager keeps this record: beside its model store.
    #[must_use]
    pub fn default_path() -> Option<std::path::PathBuf> {
        xdg_paths::xdg::data_home().map(|data| data.join("tilo/models").join(RUNTIME_FILE))
    }

    /// Read the manager's record. Missing or malformed reads as "nothing
    /// installed", never as a usable endpoint.
    #[must_use]
    pub fn load() -> Self {
        Self::default_path().map_or_else(Self::default, |path| Self::load_at(&path))
    }

    #[must_use]
    pub fn load_at(path: &Path) -> Self {
        let mut runtime: Self = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        // Only the manager's own loopback profile may occupy this record.
        runtime.profile = runtime.profile.filter(|profile| {
            profile.is_model_manager() && profile.is_device_local() && profile.credential.is_none()
        });
        runtime
    }

    /// Replace the record atomically. Only the model manager calls this.
    ///
    /// # Errors
    ///
    /// Returns an error if the record cannot be serialised or written.
    pub fn save_at(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        atomic_file::write(path, &bytes)
    }
}

/// The manager's record file name inside its model store.
pub const RUNTIME_FILE: &str = "runtime.json";

/// The person's provider profiles and deliberate default, with this device's
/// local runtime profile merged in on load. Kept separate from a task: tasks
/// capture a chosen model at start so changing this default does not change
/// work already in progress.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub profiles: Vec<ProviderProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
}

impl ProviderConfig {
    /// The person's profiles, this computer's own servers, and the model
    /// manager's profile of the models it supervises here.
    #[must_use]
    pub fn load() -> Self {
        let section = settings_store::load_section(SETTINGS_SECTION);
        Self::from_section(&section).with_runtime(LocalRuntime::load())
    }

    /// The settings at `path`, without the model manager's record.
    #[must_use]
    pub fn load_at(path: &Path) -> Self {
        Self::from_section(&settings_store::load_section_at(path, SETTINGS_SECTION))
    }

    /// Replace the model manager's profile with `runtime`'s.
    #[must_use]
    pub fn with_runtime(mut self, runtime: LocalRuntime) -> Self {
        self.profiles.retain(|profile| !profile.is_model_manager());
        if let Some(local) = runtime.profile {
            self.profiles.push(local);
        }
        self
    }

    fn from_section(section: &Map<String, Value>) -> Self {
        let mut config: Self = section
            .get(PROFILES_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value::<Self>(value).ok())
            .unwrap_or_default();
        // The personal leaf never carries anything on this computer.
        config.profiles.retain(|profile| {
            !profile.is_model_manager() && profile.location != EndpointLocation::ThisDevice
        });
        let servers: Vec<ProviderProfile> = section
            .get(THIS_DEVICE_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        config.profiles.extend(
            servers
                .into_iter()
                .filter(ProviderProfile::is_user_local_server),
        );
        config.sanitised()
    }

    /// Merge the personal profiles into the shared settings file. The device
    /// runtime profile is never written here, and no secret can enter the
    /// document because a profile holds only a [`Credential`] reference.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be serialised or the
    /// shared settings document cannot be updated.
    pub fn save(&self) -> std::io::Result<()> {
        settings_store::update_section(SETTINGS_SECTION, self.personal_section()?)
    }

    /// Save this configuration to the shared settings document at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be serialised or the
    /// settings document at `path` cannot be updated.
    pub fn save_at(&self, path: &Path) -> std::io::Result<()> {
        settings_store::update_section_at(path, SETTINGS_SECTION, self.personal_section()?)
    }

    fn personal_section(&self) -> std::io::Result<Map<String, Value>> {
        let all = self.clone().sanitised();
        let servers: Vec<&ProviderProfile> = all
            .profiles
            .iter()
            .filter(|profile| profile.is_user_local_server())
            .collect();
        let mut section = Map::new();
        section.insert(
            THIS_DEVICE_KEY.to_string(),
            serde_json::to_value(servers).map_err(std::io::Error::other)?,
        );
        let mut personal = all.clone();
        personal.profiles.retain(|profile| {
            !profile.is_model_manager() && profile.location != EndpointLocation::ThisDevice
        });
        section.insert(
            PROFILES_KEY.to_string(),
            serde_json::to_value(personal).map_err(std::io::Error::other)?,
        );
        Ok(section)
    }

    /// Validate storage and runtime-safe invariants, returning user-facing
    /// diagnostics. Invalid imported records remain visible in Settings but
    /// are never offered as an active agent provider.
    pub fn validation_errors(&self) -> Vec<ProfileIssue> {
        let mut issues = Vec::new();
        let mut ids = BTreeSet::new();
        for profile in &self.profiles {
            if profile.id.trim().is_empty() || !ids.insert(profile.id.clone()) {
                issues.push(ProfileIssue::new(&profile.id, "Profile needs a unique id."));
            }
            if profile.label.trim().is_empty() {
                issues.push(ProfileIssue::new(&profile.id, "Profile needs a name."));
            }
            if profile.endpoint.trim().is_empty() {
                issues.push(ProfileIssue::new(&profile.id, "Profile needs an endpoint."));
            } else if !is_safe_endpoint(&profile.endpoint, profile.location) {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "Endpoint must be HTTPS unless it is a local or private endpoint.",
                ));
            }
            if profile.is_user_local_server() && !is_loopback_endpoint(&profile.endpoint) {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "A server on this computer needs a localhost address.",
                ));
            }
            if profile
                .credential
                .as_ref()
                .is_some_and(|credential| !credential.is_valid())
            {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "Credential reference is incomplete.",
                ));
            }
            if matches!(profile.credential, Some(Credential::Account { .. }))
                && !profile.protocol.supports_account_sign_in()
            {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "This provider does not accept an account sign-in.",
                ));
            }
            let model_ids: BTreeSet<_> =
                profile.models.iter().map(|model| model.id.trim()).collect();
            if model_ids.len() != profile.models.len() || model_ids.contains("") {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "Every model needs a unique id.",
                ));
            }
            if profile.reasoning.as_ref().is_some_and(|reasoning| {
                reasoning.param != ReasoningParam::Omit
                    && !REASONING_EFFORTS.contains(&reasoning.effort.as_str())
            }) {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "Reasoning effort must be a supported value.",
                ));
            }
            if profile.omit_params.iter().any(|param| {
                param.is_empty()
                    || param.len() > 64
                    || !param.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
                    || REQUIRED_PARAMS.contains(&param.as_str())
            }) {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "Omitted request fields must be plain optional field names.",
                ));
            }
            if profile
                .default_model
                .as_ref()
                .is_some_and(|id| !model_ids.contains(id.as_str()))
            {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "Default model is not in this profile.",
                ));
            }
        }
        if self
            .default_profile
            .as_ref()
            .is_some_and(|id| !self.profiles.iter().any(|p| &p.id == id && p.enabled))
        {
            issues.push(ProfileIssue::new(
                "default",
                "Choose an enabled default provider.",
            ));
        }
        issues
    }

    pub fn active_profile(&self) -> Option<&ProviderProfile> {
        self.default_profile.as_ref().and_then(|id| {
            self.profiles
                .iter()
                .find(|profile| profile.id == *id && profile.enabled)
        })
    }

    pub fn profile(&self, id: &str) -> Option<&ProviderProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// This device's model-manager profile, when any local model is installed.
    pub fn local_profile(&self) -> Option<&ProviderProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.is_model_manager() && profile.is_device_local())
    }

    pub fn upsert(&mut self, profile: ProviderProfile) {
        if let Some(existing) = self
            .profiles
            .iter_mut()
            .find(|existing| existing.id == profile.id)
        {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<ProviderProfile> {
        let index = self.profiles.iter().position(|profile| profile.id == id)?;
        let removed = self.profiles.remove(index);
        if self.default_profile.as_deref() == Some(id) {
            self.default_profile = None;
        }
        Some(removed)
    }

    fn sanitised(mut self) -> Self {
        // Do not turn unknown/malformed input into a usable provider.
        self.profiles
            .retain(|profile| !profile.id.trim().is_empty());
        for profile in &mut self.profiles {
            profile.models.retain(|model| !model.id.trim().is_empty());
            if profile
                .default_model
                .as_ref()
                .is_some_and(|default| !profile.models.iter().any(|model| &model.id == default))
            {
                profile.default_model = None;
            }
        }
        self
    }
}

/// A profile validation diagnosis fit for an inline Settings status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileIssue {
    pub profile_id: String,
    pub message: String,
}

impl ProfileIssue {
    fn new(profile_id: &str, message: &str) -> Self {
        Self {
            profile_id: profile_id.to_string(),
            message: message.to_string(),
        }
    }
}

fn is_safe_endpoint(endpoint: &str, location: EndpointLocation) -> bool {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() || endpoint.contains(char::is_whitespace) {
        return false;
    }
    if location.needs_tls() {
        endpoint.starts_with("https://")
    } else {
        endpoint.starts_with("https://") || endpoint.starts_with("http://")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_shape_is_profile_configuration_not_host_inference() {
        let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
        profile.label = "OpenRouter".into();
        profile.endpoint = "https://openrouter.ai/api/v1".into();
        profile.models = vec![ModelDescriptor::new("openai/gpt-oss-safeguard-20b")];
        profile.token_limit_param = Some(TokenLimitParam::MaxTokens);
        profile.reasoning = Some(ReasoningSetting {
            param: ReasoningParam::ReasoningObject,
            effort: "low".into(),
        });
        let config = ProviderConfig {
            profiles: vec![profile.clone()],
            default_profile: None,
        };
        assert!(config.validation_errors().is_empty());
        let encoded = serde_json::to_value(&config).unwrap();
        assert_eq!(encoded["profiles"][0]["token_limit_param"], "max_tokens");
        assert_eq!(
            encoded["profiles"][0]["reasoning"],
            serde_json::json!({"param": "reasoning.effort", "effort": "low"})
        );
        let decoded: ProviderConfig = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, config);

        let mut invalid = profile.clone();
        invalid.reasoning = Some(ReasoningSetting {
            param: ReasoningParam::ReasoningEffort,
            effort: "maximal".into(),
        });
        let config = ProviderConfig {
            profiles: vec![invalid],
            default_profile: None,
        };
        assert_eq!(config.validation_errors().len(), 1);
        let mut omitted = profile.clone();
        omitted.reasoning = Some(ReasoningSetting {
            param: ReasoningParam::Omit,
            effort: String::new(),
        });
        let config = ProviderConfig {
            profiles: vec![omitted],
            default_profile: None,
        };
        assert!(config.validation_errors().is_empty());

        for bad in ["messages", "Stream", ""] {
            let mut invalid = profile.clone();
            invalid.omit_params = vec![bad.into()];
            let config = ProviderConfig {
                profiles: vec![invalid],
                default_profile: None,
            };
            assert_eq!(config.validation_errors().len(), 1, "{bad:?}");
        }
        let mut cerebras = profile.clone();
        cerebras.omit_params = vec!["stream_options".into(), "parallel_tool_calls".into()];
        let config = ProviderConfig {
            profiles: vec![cerebras],
            default_profile: None,
        };
        assert!(config.validation_errors().is_empty());

        // Hosted endpoints still require TLS, whatever the host.
        let mut plain = profile;
        plain.endpoint = "http://openrouter.ai/api/v1".into();
        let config = ProviderConfig {
            profiles: vec![plain],
            default_profile: None,
        };
        assert!(!config.validation_errors().is_empty());
        // Absent fields keep older settings documents byte-compatible.
        let minimal = serde_json::to_value(ProviderProfile::local_runtime(None)).unwrap();
        assert!(minimal.get("reasoning").is_none() && minimal.get("token_limit_param").is_none());
    }

    /// The device runtime is OpenAI-compatible and plainly local, hosted profiles hold only a vault
    /// reference, a hosted HTTP endpoint is rejected but loopback HTTP is not, and a bad default is
    /// not active.
    #[test]
    fn provider_profiles_are_plainly_local_or_vault_backed_https() {
        // Phase: local_runtime_profile_is_openai_compatible_and_plainly_local.
        {
            let profile = ProviderProfile::local_runtime(Some(
                "/run/user/1000/tilo/models/a/api.sock".into(),
            ));
            assert_eq!(profile.protocol, ProviderProtocol::OpenAiCompatible);
            assert_eq!(profile.location, EndpointLocation::ThisDevice);
            assert!(profile.is_device_local());
            assert!(profile.credential.is_none());
        }

        // Phase: hosted_profiles_hold_only_a_vault_reference.
        {
            for protocol in ProviderProtocol::ALL {
                let profile = ProviderProfile::hosted(*protocol);
                let json = serde_json::to_string(&profile).unwrap();
                assert!(json.contains("tilo.secret-service"));
                assert!(json.contains("agent-provider/"));
                assert!(!json.contains("sk-ant-"));
            }
        }

        // Phase: hosted_http_endpoint_is_rejected_but_loopback_http_is_not.
        {
            let mut profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
            profile.endpoint = "http://api.example.test".into();
            let config = ProviderConfig {
                profiles: vec![profile],
                ..ProviderConfig::default()
            };
            assert!(!config.validation_errors().is_empty());
            let local = ProviderConfig {
                profiles: vec![ProviderProfile::local_runtime(Some(
                    "/run/user/1000/tilo/models/a/api.sock".into(),
                ))],
                ..ProviderConfig::default()
            };
            assert!(local.validation_errors().is_empty());
        }

        // Phase: bad_default_is_not_active.
        {
            let profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
            let config = ProviderConfig {
                default_profile: Some("gone".into()),
                profiles: vec![profile],
            };
            assert!(config.active_profile().is_none());
            assert!(
                config
                    .validation_errors()
                    .iter()
                    .any(|issue| issue.profile_id == "default")
            );
        }
    }

    #[test]
    fn only_azure_accepts_an_account_sign_in() {
        let mut azure = ProviderProfile::hosted(ProviderProtocol::AzureOpenAi);
        azure.credential = Some(Credential::Account {
            account_id: "entra-person".into(),
        });
        let mut anthropic = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        anthropic.credential = azure.credential.clone();
        let config = ProviderConfig {
            profiles: vec![azure.clone(), anthropic.clone()],
            ..ProviderConfig::default()
        };
        let issues = config.validation_errors();
        assert!(issues.iter().all(|issue| issue.profile_id != azure.id));
        assert!(issues.iter().any(|issue| issue.profile_id == anthropic.id));
    }

    #[test]
    fn each_record_is_stored_where_its_scope_says() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut appearance = Map::new();
        appearance.insert("theme".into(), serde_json::json!("dark"));
        settings_store::update_section_at(&path, "appearance", appearance).unwrap();

        let hosted = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
        let mut ollama = ProviderProfile::new(
            ProviderProtocol::OpenAiCompatible,
            EndpointLocation::ThisDevice,
        );
        ollama.endpoint = "http://127.0.0.1:11434/v1".into();
        let mut local =
            ProviderProfile::local_runtime(Some("/run/user/1000/tilo/models/a/api.sock".into()));
        local.models.push(ModelDescriptor::new("qwen"));
        let config = ProviderConfig {
            default_profile: Some(hosted.id.clone()),
            // A caller that saves the merged view never writes the manager's
            // profile into a settings leaf.
            profiles: vec![hosted.clone(), ollama.clone(), local.clone()],
        };
        config.save_at(&path).unwrap();

        let loaded = ProviderConfig::load_at(&path);
        assert_eq!(loaded.profiles, vec![hosted.clone(), ollama.clone()]);
        let document = settings_store::load_at(&path);
        let section = document[SETTINGS_SECTION].as_object().unwrap();
        let personal = section[PROFILES_KEY].to_string();
        assert!(personal.contains(&hosted.id));
        assert!(!personal.contains(&ollama.id));
        assert!(!personal.contains(LOCAL_PROFILE_ID));
        assert!(section[THIS_DEVICE_KEY].to_string().contains("11434"));
        assert!(document.contains_key("appearance"));

        let runtime_path = dir.path().join(RUNTIME_FILE);
        LocalRuntime {
            hardware: None,
            profile: Some(local.clone()),
        }
        .save_at(&runtime_path)
        .unwrap();
        let merged = loaded.with_runtime(LocalRuntime::load_at(&runtime_path));
        assert_eq!(merged.local_profile(), Some(&local));
        assert!(!ollama.is_supervised());
        assert!(ollama.is_user_local_server());
    }

    #[test]
    fn a_server_on_this_computer_must_listen_on_localhost() {
        let mut lan = ProviderProfile::new(
            ProviderProtocol::OpenAiCompatible,
            EndpointLocation::ThisDevice,
        );
        lan.endpoint = "http://192.168.1.4:11434/v1".into();
        let config = ProviderConfig {
            profiles: vec![lan.clone()],
            default_profile: None,
        };
        assert!(
            config
                .validation_errors()
                .iter()
                .any(|issue| issue.profile_id == lan.id)
        );
    }

    #[test]
    fn a_non_loopback_profile_is_never_accepted_as_the_device_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RUNTIME_FILE);
        let mut remote = ProviderProfile::local_runtime(None);
        remote.endpoint = "http://192.168.1.4:8000/v1".into();
        LocalRuntime {
            hardware: None,
            profile: Some(remote),
        }
        .save_at(&path)
        .unwrap();
        assert!(LocalRuntime::load_at(&path).profile.is_none());
    }

    #[test]
    fn a_hosted_or_stopped_profile_never_offers_a_pinned_local_model() {
        let model = ModelDescriptor {
            revision: Some("a".repeat(40)),
            capabilities: [ModelCapability::TraceReview].into(),
            ..ModelDescriptor::new("m")
        };
        let mut local =
            ProviderProfile::local_runtime(Some("/run/user/1000/tilo/models/a/api.sock".into()));
        local.models = vec![model];
        assert!(
            local
                .pinned_local_model("m", ModelCapability::TraceReview)
                .is_some()
        );
        assert!(
            local
                .pinned_local_model("m", ModelCapability::Vision)
                .is_none()
        );
        let mut hosted = local.clone();
        hosted.location = EndpointLocation::Hosted;
        assert!(
            hosted
                .pinned_local_model("m", ModelCapability::TraceReview)
                .is_none()
        );
        let mut stopped = local;
        stopped.enabled = false;
        assert!(
            stopped
                .pinned_local_model("m", ModelCapability::TraceReview)
                .is_none()
        );
    }

    #[test]
    fn loopback_and_revision_checks_are_strict() {
        assert!(is_loopback_endpoint("http://127.0.0.1:8123/v1"));
        assert!(is_loopback_endpoint("http://[::1]:8123/v1"));
        assert!(!is_loopback_endpoint("http://192.168.1.20:8123/v1"));
        assert!(!is_loopback_endpoint("http://localhost.evil.test:8123/v1"));
        assert!(is_pinned_revision(&"a".repeat(40)));
        assert!(!is_pinned_revision("main"));
        assert!(!is_pinned_revision(&"A".repeat(40)));
    }
}
