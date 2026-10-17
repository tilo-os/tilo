//! Provider-neutral configuration for Tilo agents.
//!
//! This crate stores *descriptions* of model providers, never a
//! bearer token. A profile can point at a system-vault item via
//! [`SecretReference`], or name a sign-in held by account custody via
//! [`SignInCredential`], while the process that owns the secret service is
//! responsible for resolving either at call time. Keeping this
//! boundary here makes the same profile safe to render in Settings, sync as
//! non-secret device intent, or hand to a sandboxed agent.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod local;
/// Single-shot model calls in either provider dialect.
pub mod transform;

/// The `settings.json` section owned by this crate.
pub const SETTINGS_SECTION: &str = "agent_providers";
/// The key holding the providers of this device and its network, and the
/// default: they stay on the device.
const CONFIG_KEY: &str = "config";
/// The key holding the hosted providers: they name no device, so they roam
/// with the person.
const HOSTED_KEY: &str = "hosted";

/// The request dialect spoken by a provider endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum ProviderProtocol {
    /// `/v1/chat/completions` and related OpenAI-compatible endpoints.
    #[default]
    OpenAiCompatible,
    /// Anthropic's `/v1/messages` API.
    Anthropic,
}

settings_store::read_through_json_value!(ProviderProtocol);

impl ProviderProtocol {
    pub const ALL: &'static [Self] = &[Self::OpenAiCompatible, Self::Anthropic];

    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAI-compatible",
            Self::Anthropic => "Anthropic",
        }
    }
}

/// Where an endpoint runs. This is an explicit trust cue in the UI: a local
/// vLLM server must remain distinct from a hosted API even though it shares the
/// `OpenAI` wire protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum EndpointLocation {
    /// A provider-operated internet endpoint.
    #[default]
    Hosted,
    /// A loopback endpoint on this Tilo device, e.g. vLLM or Ollama.
    ThisDevice,
    /// An endpoint on a LAN, VPN, or a user-managed server.
    PrivateNetwork,
}

settings_store::read_through_json_value!(EndpointLocation);

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
#[serde(remote = "Self")]
pub struct SecretReference {
    /// The broker/vault namespace, usually `tilo.secret-service`.
    pub vault: String,
    /// Opaque item locator within `vault`; never the secret value.
    pub locator: String,
}

settings_store::read_through_json_value!(SecretReference);

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

/// The issuer of a [`SignInCredential`] that signs in with Google.
pub const GOOGLE_SIGN_IN: &str = "google";

/// A sign-in held by account custody in place of an API key: the gateway
/// mints a short-lived access token from it for each request and refreshes
/// it when due. The grant itself lives in this device's vault under
/// [`sign_in_grant_id`], derived from the profile id so a profile cannot
/// name another account's grant (Gmail's, say).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct SignInCredential {
    /// Who signed the person in; [`GOOGLE_SIGN_IN`] is the one supported.
    pub issuer: String,
    /// The Google Cloud project billed for requests made with a person's own
    /// credentials, sent as `x-goog-user-project`. Without it Google bills
    /// the project of Tilo's `OAuth` client, which the person cannot use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_project: Option<String>,
}

settings_store::read_through_json_value!(SignInCredential);

impl SignInCredential {
    pub fn google(billing_project: impl Into<String>) -> Self {
        Self {
            issuer: GOOGLE_SIGN_IN.to_string(),
            billing_project: Some(billing_project.into()),
        }
    }
}

/// The vault id of `profile_id`'s sign-in grant. Its own namespace, apart
/// from the API keys (`agent-provider-…`), so neither can address the other.
pub fn sign_in_grant_id(profile_id: &str) -> String {
    format!("agent-model-grant-{profile_id}")
}

/// Vertex AI's default location; Google's newest models are served there
/// first, some only there.
pub const VERTEX_DEFAULT_LOCATION: &str = "global";
/// The model a new Vertex AI provider offers.
pub const VERTEX_DEFAULT_MODEL: &str = "gemini-4-argon";

/// Whether `project` is a Google Cloud project id: lowercase letters,
/// digits and hyphens, or a domain-scoped id (`example.com:project`).
pub fn is_cloud_project_id(project: &str) -> bool {
    !project.is_empty()
        && project.len() <= 100
        && project.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-.:".contains(&byte)
        })
}

/// Whether `location` is a Vertex AI location name (`global`,
/// `us-central1`, `europe-west4`).
pub fn is_vertex_location(location: &str) -> bool {
    !location.is_empty()
        && location.len() <= 40
        && location
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !location.starts_with('-')
}

/// Vertex AI's OpenAI-compatible endpoint for `project` in `location`. The
/// global location is served from the global host, every other one from
/// its regional host.
pub fn vertex_endpoint(project: &str, location: &str) -> String {
    let host = if location == VERTEX_DEFAULT_LOCATION {
        "aiplatform.googleapis.com".to_string()
    } else {
        format!("{location}-aiplatform.googleapis.com")
    };
    format!("https://{host}/v1/projects/{project}/locations/{location}/endpoints/openapi")
}

/// The project and location of a [`vertex_endpoint`], or `None` for any
/// other endpoint.
pub fn vertex_target(endpoint: &str) -> Option<(String, String)> {
    let rest = endpoint.trim().strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix("/chat/completions").unwrap_or(path);
    let path = path.strip_prefix("v1/projects/")?;
    let (project, path) = path.split_once("/locations/")?;
    let location = path.strip_suffix("/endpoints/openapi")?;
    let canonical = vertex_endpoint(project, location);
    (is_cloud_project_id(project)
        && is_vertex_location(location)
        && canonical.strip_prefix("https://")?.split_once('/')?.0 == host)
        .then(|| (project.to_string(), location.to_string()))
}

/// The id Vertex AI's OpenAI-compatible endpoint takes for `model`: a
/// publisher-qualified id as given, a bare Gemini name under `google/`.
pub fn vertex_model_id(model: &str) -> String {
    let model = model.trim();
    if model.contains('/') {
        model.to_string()
    } else {
        format!("google/{model}")
    }
}

/// One model advertised by a profile. A model list is metadata, not a
/// capability grant: individual agent tasks still require their own action
/// permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct ModelDescriptor {
    /// Provider-native model id sent over the wire.
    pub id: String,
    /// User-facing label. Defaults to `id` for imported/minimal profiles.
    pub label: String,
    /// Best known input-context size. `None` means unknown, not unlimited.
    pub context_window: Option<u32>,
    /// Optional Settings-owned request policy. Runtime context is a configured
    /// deployment limit, separate from this model's source context capacity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_limits: Option<ModelRequestLimits>,
    /// Immutable release revision when this model comes from the signed local
    /// model catalogue. Hosted providers often cannot supply one, so absence
    /// means "not attested", never "latest".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// A declared capability, not an inference from a model's friendly name.
    /// For locally managed models this value is copied from the signed
    /// catalogue; hosted profiles remain an explicit user/provider claim.
    pub vision: bool,
    /// Signed declaration that this exact local model exposes the bounded
    /// Photos face-detection/embedding/clustering protocol. This is separate
    /// from generic vision: image descriptions must never enable biometrics.
    #[serde(default)]
    pub face_recognition: bool,
    pub tool_calling: bool,
}

settings_store::read_through_json_value!(ModelDescriptor);

impl Default for ModelDescriptor {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            context_window: None,
            request_limits: None,
            revision: None,
            vision: false,
            face_recognition: false,
            tool_calling: true,
        }
    }
}

/// Explicit request limits for one selected model; not a capability or runtime
/// attestation. None on a descriptor preserves legacy request shaping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct ModelRequestLimits {
    pub max_output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_context_tokens: Option<u32>,
    /// Owner-configured Plan deadline ceiling; absent preserves 60 seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_timeout_ms: Option<u32>,
}

settings_store::read_through_json_value!(ModelRequestLimits);

impl ModelRequestLimits {
    /// Validate bounded settings without estimating the token count of a prompt.
    ///
    /// # Errors
    /// Returns a diagnostic for an invalid allowance or contradictory context.
    pub fn validate(&self, source_context: Option<u32>) -> Result<(), &'static str> {
        if self.max_output_tokens == 0 || self.max_output_tokens > 65_536 {
            return Err("Output allowance must be between 1 and 65536 tokens.");
        }
        if let Some(context) = self.runtime_context_tokens
            && (context == 0
                || context > 1_048_576
                || self.max_output_tokens >= context
                || source_context.is_some_and(|source| context > source))
        {
            return Err("Runtime context must fit the model and leave room beyond output.");
        }
        if self
            .plan_timeout_ms
            .is_some_and(|timeout| timeout == 0 || timeout > 900_000)
        {
            return Err("Plan timeout must be between 1 and 900000 milliseconds.");
        }
        Ok(())
    }
}

impl ModelDescriptor {
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

/// A configured model-provider endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct ProviderProfile {
    pub id: String,
    pub label: String,
    pub protocol: ProviderProtocol,
    pub endpoint: String,
    pub location: EndpointLocation,
    /// Absent for an unauthenticated local endpoint. The secret
    /// itself is never accepted or serialised by this type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<SecretReference>,
    /// A sign-in in place of an API key (Google Vertex AI). Absent in
    /// settings written before sign-in existed, so they read unchanged; a
    /// reader that predates it ignores the field and asks for a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sign_in: Option<SignInCredential>,
    pub enabled: bool,
    pub models: Vec<ModelDescriptor>,
    /// The default model id within `models`. Missing/unknown is represented
    /// by `None`, so callers prompt instead of silently choosing a model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// How an OpenAI-compatible request names its output-token limit. The
    /// model gateway owns the outbound request and renames whichever limit the
    /// caller sent to this one. `None` sends the caller's name unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_limit_param: Option<TokenLimitParam>,
    /// The reasoning setting the gateway sends with every OpenAI-compatible
    /// request of this profile, whatever the caller asked for. `None` sends
    /// nothing (the provider's default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningSetting>,
    /// Top-level request fields this provider rejects, removed by the gateway
    /// after it has shaped the request (e.g. `stream_options`,
    /// `parallel_tool_calls`). Each is a plain JSON key; `model`,
    /// `messages`, `tools` and `tool_choice` cannot be listed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub omit_params: Vec<String>,
}

settings_store::read_through_json_value!(ProviderProfile);

/// Request fields a profile may not strip: without them there is no request.
pub const REQUIRED_PARAMS: &[&str] = &["model", "messages", "tools", "tool_choice"];

/// The output-token-limit field of an OpenAI-compatible provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum TokenLimitParam {
    #[serde(rename = "max_tokens")]
    MaxTokens,
    #[serde(rename = "max_completion_tokens")]
    MaxCompletionTokens,
}

settings_store::read_through_json_value!(TokenLimitParam);

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
#[serde(remote = "Self")]
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

settings_store::read_through_json_value!(ReasoningParam);

/// A profile's reasoning setting, applied by the gateway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct ReasoningSetting {
    pub param: ReasoningParam,
    /// One of [`REASONING_EFFORTS`]; unused when `param` is `none`. Not every
    /// model accepts every value (GPT-5.4 rejects `minimal`, gpt-oss cannot
    /// disable reasoning so its lowest is `low`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effort: String,
}

settings_store::read_through_json_value!(ReasoningSetting);

/// The reasoning efforts a profile may name.
pub const REASONING_EFFORTS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh"];

#[cfg(feature = "model")]
tilo_resource::json_model!(ProviderProfile);

impl Default for ProviderProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            protocol: ProviderProtocol::default(),
            endpoint: String::new(),
            location: EndpointLocation::default(),
            credential: None,
            sign_in: None,
            enabled: true,
            models: Vec::new(),
            default_model: None,
            token_limit_param: None,
            reasoning: None,
            omit_params: Vec::new(),
        }
    }
}

impl ProviderProfile {
    /// A blank hosted profile with an allocated id and a vault reference. The
    /// caller can open a system credential prompt for that reference; no UI
    /// needs to retain the typed API key in its widget state or CRDT.
    pub fn hosted(protocol: ProviderProtocol) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let (label, endpoint, default_model) = match protocol {
            ProviderProtocol::OpenAiCompatible => (
                "OpenAI-compatible provider",
                "https://api.openai.com/v1",
                None,
            ),
            ProviderProtocol::Anthropic => ("Anthropic", "https://api.anthropic.com", None),
        };
        Self {
            id: id.clone(),
            label: label.to_string(),
            protocol,
            endpoint: endpoint.to_string(),
            location: EndpointLocation::Hosted,
            credential: Some(SecretReference::provider_api_key(&id)),
            sign_in: None,
            enabled: true,
            models: Vec::new(),
            default_model,
            token_limit_param: None,
            reasoning: None,
            omit_params: Vec::new(),
        }
    }

    /// An OpenAI-compatible local profile suitable for vLLM. It is
    /// unauthenticated by default because a loopback server need not have a
    /// token; users can attach a vault reference if their deployment requires
    /// one.
    pub fn local_vllm() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id,
            label: "Local vLLM".to_string(),
            protocol: ProviderProtocol::OpenAiCompatible,
            endpoint: "http://127.0.0.1:8000/v1".to_string(),
            location: EndpointLocation::ThisDevice,
            credential: None,
            sign_in: None,
            enabled: true,
            models: Vec::new(),
            default_model: None,
            token_limit_param: None,
            reasoning: None,
            omit_params: Vec::new(),
        }
    }

    /// A Google Vertex AI profile for `project` in `location`, offering
    /// `model`, signed in with Google rather than an API key. It speaks
    /// Vertex AI's OpenAI-compatible endpoint, which accepts every field the
    /// gateway sends (`stream_options`, `parallel_tool_calls`,
    /// `max_completion_tokens`, `reasoning_effort`), so nothing is omitted.
    pub fn google_vertex(project: &str, location: &str, model: &str) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let model = ModelDescriptor {
            label: model.trim().to_string(),
            ..ModelDescriptor::new(vertex_model_id(model))
        };
        Self {
            id,
            label: "Google Vertex AI".to_string(),
            protocol: ProviderProtocol::OpenAiCompatible,
            endpoint: vertex_endpoint(project, location),
            location: EndpointLocation::Hosted,
            credential: None,
            sign_in: Some(SignInCredential::google(project)),
            enabled: true,
            default_model: Some(model.id.clone()),
            models: vec![model],
            token_limit_param: None,
            reasoning: None,
            omit_params: Vec::new(),
        }
    }

    /// Whether this profile signs in with Google instead of using a key.
    pub fn signs_in_with_google(&self) -> bool {
        self.sign_in
            .as_ref()
            .is_some_and(|sign_in| sign_in.issuer == GOOGLE_SIGN_IN)
    }

    pub fn display_label(&self) -> &str {
        if self.label.trim().is_empty() {
            &self.id
        } else {
            &self.label
        }
    }

    pub fn is_local_vllm_candidate(&self) -> bool {
        self.protocol == ProviderProtocol::OpenAiCompatible
            && self.location == EndpointLocation::ThisDevice
            && (self.endpoint.contains(":8000") || self.label.to_ascii_lowercase().contains("vllm"))
    }
}

/// The hosted providers of a configuration, as they roam with the person:
/// each names an endpoint on the internet and a reference to its key, never
/// the key, which each device keeps in its own vault.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct HostedProfiles {
    pub profiles: Vec<ProviderProfile>,
}

settings_store::read_through_json_value!(HostedProfiles);

/// A portable profile list and the user's deliberate default. Kept separate
/// from a task: tasks capture a chosen model at start so changing this default
/// does not change work already in progress.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct ProviderConfig {
    pub profiles: Vec<ProviderProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
}

settings_store::read_through_json_value!(ProviderConfig);

impl ProviderConfig {
    pub fn load() -> Self {
        let section = settings_store::load_section(SETTINGS_SECTION);
        Self::from_section(&section)
    }

    pub fn load_at(path: &Path) -> Self {
        let section = settings_store::load_at(path)
            .remove(SETTINGS_SECTION)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        Self::from_section(&section)
    }

    fn from_section(section: &Map<String, Value>) -> Self {
        let local = section
            .get(CONFIG_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value::<Self>(value).ok())
            .unwrap_or_default();
        let hosted = section
            .get(HOSTED_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value::<HostedProfiles>(value).ok())
            .unwrap_or_default();
        Self::from_stored(local, hosted).sanitised()
    }

    /// This configuration as it is stored: the providers of this device and
    /// its network with the default, which stay on the device, and the
    /// hosted providers, which roam with the person.
    #[must_use]
    pub fn stored(self) -> (Self, HostedProfiles) {
        let (hosted, local): (Vec<_>, Vec<_>) = self
            .profiles
            .into_iter()
            .partition(|profile| profile.location == EndpointLocation::Hosted);
        (
            Self {
                profiles: local,
                default_profile: self.default_profile,
            },
            HostedProfiles { profiles: hosted },
        )
    }

    /// The configuration stored as `local` and `hosted`; a profile stored in
    /// both is the local one.
    #[must_use]
    pub fn from_stored(local: Self, hosted: HostedProfiles) -> Self {
        let mut profiles = local.profiles;
        for profile in hosted.profiles {
            if !profiles.iter().any(|known| known.id == profile.id) {
                profiles.push(profile);
            }
        }
        Self {
            profiles,
            default_profile: local.default_profile,
        }
    }

    /// The section's two keys for this configuration.
    fn section(&self) -> std::io::Result<Map<String, Value>> {
        let (local, hosted) = self.clone().sanitised().stored();
        let mut section = Map::new();
        section.insert(
            CONFIG_KEY.to_string(),
            serde_json::to_value(local).map_err(std::io::Error::other)?,
        );
        section.insert(
            HOSTED_KEY.to_string(),
            serde_json::to_value(hosted).map_err(std::io::Error::other)?,
        );
        Ok(section)
    }

    /// Merge our single configuration payload into the shared settings file.
    /// Other Settings panes retain their sections, and no secret can enter the
    /// document because `ProviderProfile` exposes a reference only.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be serialised or the
    /// shared settings document cannot be updated.
    pub fn save(&self) -> std::io::Result<()> {
        settings_store::update_section(SETTINGS_SECTION, self.section()?)
    }

    /// Save this configuration to the shared settings document at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be serialised or the
    /// settings document at `path` cannot be updated.
    pub fn save_at(&self, path: &Path) -> std::io::Result<()> {
        settings_store::update_section_at(path, SETTINGS_SECTION, self.section()?)
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
            if profile
                .credential
                .as_ref()
                .is_some_and(|reference| !reference.is_valid())
            {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "Credential reference is incomplete.",
                ));
            }
            sign_in_issues(profile, &mut issues);
            let model_ids: BTreeSet<_> =
                profile.models.iter().map(|model| model.id.trim()).collect();
            if model_ids.len() != profile.models.len() || model_ids.contains("") {
                issues.push(ProfileIssue::new(
                    &profile.id,
                    "Every model needs a unique id.",
                ));
            }
            for model in &profile.models {
                if let Some(limits) = &model.request_limits
                    && let Err(message) = limits.validate(model.context_window)
                {
                    issues.push(ProfileIssue::new(&profile.id, message));
                }
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

    /// The configuration as it is stored and read: profiles without an id,
    /// models without an id and defaults naming no model are dropped.
    #[must_use]
    pub fn sanitised(mut self) -> Self {
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

/// What is wrong with `profile`'s sign-in, if it has one.
fn sign_in_issues(profile: &ProviderProfile, issues: &mut Vec<ProfileIssue>) {
    if let Some(sign_in) = &profile.sign_in {
        if profile.credential.is_some() {
            issues.push(ProfileIssue::new(
                &profile.id,
                "Profile uses both an API key and a sign-in.",
            ));
        }
        if sign_in.issuer != GOOGLE_SIGN_IN {
            issues.push(ProfileIssue::new(
                &profile.id,
                "Profile signs in with an unsupported provider.",
            ));
        }
        if sign_in
            .billing_project
            .as_deref()
            .is_some_and(|project| !is_cloud_project_id(project))
        {
            issues.push(ProfileIssue::new(
                &profile.id,
                "Billing project is not a Google Cloud project id.",
            ));
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
    fn model_request_limits_preserve_legacy_and_validate_runtime_room() {
        let legacy = ModelDescriptor::new("m");
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("request_limits")
                .is_none()
        );
        let mut profile = ProviderProfile::local_vllm();
        profile.models = vec![legacy];
        let mut config = ProviderConfig {
            profiles: vec![profile],
            default_profile: None,
        };
        for (output, context, source, valid) in [
            (512, Some(4096), Some(262_144), true),
            (512, None, None, true),
            (0, Some(4096), None, false),
            (65_537, None, None, false),
            (4096, Some(4096), None, false),
            (512, Some(1_048_577), None, false),
            (512, Some(8192), Some(4096), false),
        ] {
            config.profiles[0].models[0].context_window = source;
            config.profiles[0].models[0].request_limits = Some(ModelRequestLimits {
                max_output_tokens: output,
                runtime_context_tokens: context,
                plan_timeout_ms: None,
            });
            assert_eq!(config.validation_errors().is_empty(), valid);
        }
        for (timeout, valid) in [
            (None, true),
            (Some(1), true),
            (Some(900_000), true),
            (Some(0), false),
            (Some(900_001), false),
        ] {
            let limits = ModelRequestLimits {
                max_output_tokens: 512,
                runtime_context_tokens: Some(4096),
                plan_timeout_ms: timeout,
            };
            assert_eq!(limits.validate(None).is_ok(), valid);
            if timeout.is_none() {
                assert_eq!(
                    serde_json::to_value(&limits).unwrap(),
                    serde_json::json!({"max_output_tokens":512,"runtime_context_tokens":4096})
                );
            }
        }
        for malformed in [
            serde_json::json!(true),
            serde_json::json!("1000"),
            serde_json::json!(-1),
        ] {
            assert!(
                serde_json::from_value::<ModelRequestLimits>(serde_json::json!({
                "max_output_tokens":512,"plan_timeout_ms":malformed}))
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<ModelRequestLimits>(
                serde_json::json!({"max_output_tokens":false})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ModelRequestLimits>(
                serde_json::json!({"max_output_tokens":512,"hidden_override":true})
            )
            .is_err()
        );
    }

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
        let minimal = serde_json::to_value(ProviderProfile::local_vllm()).unwrap();
        assert!(minimal.get("reasoning").is_none() && minimal.get("token_limit_param").is_none());
    }

    /// Local vLLM is OpenAI-compatible and plainly local, hosted profiles hold only a vault
    /// reference, a hosted HTTP endpoint is rejected but loopback HTTP is not, and a bad default is
    /// not active.
    #[test]
    fn provider_profiles_are_plainly_local_or_vault_backed_https() {
        // Phase: local_vllm_is_openai_compatible_and_plainly_local.
        {
            let profile = ProviderProfile::local_vllm();
            assert_eq!(profile.protocol, ProviderProtocol::OpenAiCompatible);
            assert_eq!(profile.location, EndpointLocation::ThisDevice);
            assert!(profile.endpoint.starts_with("http://127.0.0.1"));
            assert!(profile.is_local_vllm_candidate());
            assert!(profile.credential.is_none());
        }

        // Phase: hosted_profiles_hold_only_a_vault_reference.
        {
            let profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
            let json = serde_json::to_string(&profile).unwrap();
            assert!(json.contains("tilo.secret-service"));
            assert!(json.contains("agent-provider/"));
            assert!(!json.contains("sk-ant-"));
            assert!(!json.contains("api_key\":\""));
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
                profiles: vec![ProviderProfile::local_vllm()],
                ..ProviderConfig::default()
            };
            assert!(local.validation_errors().is_empty());
        }

        // Phase: bad_default_is_not_active.
        {
            let config = ProviderConfig {
                default_profile: Some("gone".into()),
                profiles: vec![ProviderProfile::local_vllm()],
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

    /// Settings written before sign-in existed read unchanged, a sign-in
    /// round-trips, and a profile without one writes no new field.
    #[test]
    fn sign_in_profiles_are_backward_compatible_settings() {
        let older = serde_json::json!({
            "profiles": [{
                "id": "p", "label": "OpenRouter", "protocol": "open_ai_compatible",
                "endpoint": "https://openrouter.ai/api/v1", "location": "hosted",
                "credential": {"vault": "tilo.secret-service", "locator": "agent-provider/p/api-key"},
                "enabled": true, "models": [{"id": "m"}], "default_model": "m"
            }],
            "default_profile": "p"
        });
        let config: ProviderConfig = serde_json::from_value(older).unwrap();
        assert_eq!(config.profiles.len(), 1);
        assert!(config.profiles[0].sign_in.is_none());
        assert!(config.validation_errors().is_empty());
        let written = serde_json::to_value(&config).unwrap();
        assert!(written["profiles"][0].get("sign_in").is_none());

        let vertex = ProviderProfile::google_vertex("tilo-509618", "global", "gemini-4-argon");
        assert!(vertex.credential.is_none());
        assert!(vertex.signs_in_with_google());
        assert_eq!(
            vertex.default_model.as_deref(),
            Some("google/gemini-4-argon")
        );
        assert_eq!(vertex.models[0].label, "gemini-4-argon");
        let config = ProviderConfig {
            profiles: vec![vertex.clone()],
            default_profile: Some(vertex.id.clone()),
        };
        assert!(config.validation_errors().is_empty());
        let written = serde_json::to_value(&config).unwrap();
        assert_eq!(
            written["profiles"][0]["sign_in"],
            serde_json::json!({"issuer": "google", "billing_project": "tilo-509618"})
        );
        assert_eq!(
            serde_json::from_value::<ProviderConfig>(written).unwrap(),
            config
        );

        for (credential, issuer, project) in [
            (true, GOOGLE_SIGN_IN, "tilo-509618"),
            (false, "elsewhere", "tilo-509618"),
            (false, GOOGLE_SIGN_IN, "Not A Project"),
        ] {
            let mut invalid = vertex.clone();
            invalid.credential = credential.then(|| SecretReference::provider_api_key(&invalid.id));
            invalid.sign_in = Some(SignInCredential {
                issuer: issuer.into(),
                billing_project: Some(project.into()),
            });
            let config = ProviderConfig {
                profiles: vec![invalid],
                default_profile: None,
            };
            assert_eq!(config.validation_errors().len(), 1, "{issuer} {project}");
        }
        assert_ne!(sign_in_grant_id("p"), "agent-provider-p");
    }

    #[test]
    fn vertex_endpoints_name_their_project_and_location() {
        assert_eq!(
            vertex_endpoint("tilo-509618", "global"),
            "https://aiplatform.googleapis.com/v1/projects/tilo-509618/locations/global/endpoints/openapi"
        );
        assert_eq!(
            vertex_endpoint("p-1", "europe-west4"),
            "https://europe-west4-aiplatform.googleapis.com/v1/projects/p-1/locations/europe-west4/endpoints/openapi"
        );
        for (project, location) in [("tilo-509618", "global"), ("example.com:p", "us-central1")] {
            assert_eq!(
                vertex_target(&vertex_endpoint(project, location)),
                Some((project.to_string(), location.to_string()))
            );
        }
        assert_eq!(
            vertex_target(&format!(
                "{}/chat/completions",
                vertex_endpoint("p", "global")
            )),
            Some(("p".into(), "global".into()))
        );
        // A regional location on the global host, or any other endpoint, is
        // not a Vertex target.
        assert_eq!(
            vertex_target(
                "https://aiplatform.googleapis.com/v1/projects/p/locations/us-central1/endpoints/openapi"
            ),
            None
        );
        assert_eq!(vertex_target("https://api.openai.com/v1"), None);
        assert_eq!(vertex_model_id("gemini-4-argon"), "google/gemini-4-argon");
        assert_eq!(vertex_model_id(" meta/llama-4 "), "meta/llama-4");
        assert!(is_vertex_location("us-east5") && !is_vertex_location("US"));
        assert!(!is_vertex_location("a/b") && !is_vertex_location(""));
        assert!(is_cloud_project_id("tilo-509618") && !is_cloud_project_id("a/b"));
    }

    #[test]
    fn save_round_trip_preserves_other_settings_and_never_needs_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut existing = Map::new();
        existing.insert("appearance".into(), serde_json::json!({"theme": "dark"}));
        settings_store::update_section_at(
            &path,
            "appearance",
            existing
                .remove("appearance")
                .unwrap()
                .as_object()
                .cloned()
                .unwrap(),
        )
        .unwrap();
        let profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
        let config = ProviderConfig {
            default_profile: Some(profile.id.clone()),
            profiles: vec![profile],
        };
        config.save_at(&path).unwrap();
        assert_eq!(ProviderConfig::load_at(&path), config);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("appearance"));
        assert!(!body.contains("sk-live-secret"));
    }

    /// A hosted provider is stored apart from the ones on this device, so it
    /// roams with the person while a loopback endpoint and the default stay.
    #[test]
    fn hosted_providers_are_stored_apart_from_this_devices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let hosted = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        let local = ProviderProfile::local_vllm();
        let config = ProviderConfig {
            default_profile: Some(local.id.clone()),
            profiles: vec![local.clone(), hosted.clone()],
        };
        config.save_at(&path).unwrap();
        assert_eq!(ProviderConfig::load_at(&path), config);
        let document = settings_store::load_at(&path);
        let section = document[SETTINGS_SECTION].as_object().unwrap();
        let stored: ProviderConfig = serde_json::from_value(section[CONFIG_KEY].clone()).unwrap();
        let roaming: HostedProfiles = serde_json::from_value(section[HOSTED_KEY].clone()).unwrap();
        assert_eq!(stored.profiles, vec![local]);
        assert_eq!(stored.default_profile, config.default_profile);
        assert_eq!(roaming.profiles, vec![hosted]);
        assert!(settings_store::continuity::is_personal(
            "tilo.agent_providers.hosted"
        ));
        assert!(!settings_store::continuity::is_personal(
            "tilo.agent_providers.config"
        ));
    }
}

/// Typed local-speech configuration and its private worker wire format.
pub mod speech;
