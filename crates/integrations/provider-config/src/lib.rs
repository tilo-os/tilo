//! Provider-neutral configuration for Tilo agents.
//!
//! This crate stores *descriptions* of model providers, never a
//! bearer token. A profile can point at a system-vault item via
//! [`SecretReference`], or name a sign-in held by account credentials via
//! [`SignInCredential`], while the process that owns the secret service is
//! responsible for resolving either at call time. Keeping this
//! boundary here makes the same profile safe to render in Settings, sync as
//! non-secret device intent, or hand to a sandboxed agent.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// What a model can do: read from a provider's list, or known by family.
pub mod capabilities;
/// Whole model turns in either provider dialect, for clients without the gateway.
pub mod local;
pub mod management;
/// Known providers a new profile starts from.
pub mod presets;
pub mod protocol;
/// Single-shot model calls in either provider dialect.
pub mod transform;
pub mod wire;

/// The section holding the providers of this device and its network, and
/// the default: this device's.
pub const SETTINGS_SECTION: &str = "agent_providers";
/// The section holding the hosted providers: they name no device, so it is
/// the person's.
pub const HOSTED_SECTION: &str = "agent_providers_person";
/// The key holding the providers of this device and its network, and the
/// default, in [`SETTINGS_SECTION`].
const CONFIG_KEY: &str = "config";
/// The key holding the hosted providers in [`HOSTED_SECTION`].
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

    /// Where `endpoint` runs, as its address says: on this device (a
    /// loopback address or `localhost`), on a private network (a private or
    /// link-local address, a `.local` or `.lan` name), else hosted. `None`
    /// for what is not an address. A typed endpoint takes the location its
    /// address has, so a server on the person's network is not held to a
    /// hosted provider's TLS, and an internet host never escapes it.
    pub fn of_endpoint(endpoint: &str) -> Option<Self> {
        let url = url::Url::parse(endpoint.trim()).ok()?;
        Some(match url.host()? {
            url::Host::Domain(name) => {
                let name = name.trim_end_matches('.').to_ascii_lowercase();
                let last = name.rsplit('.').next().unwrap_or_default();
                if last == "localhost" {
                    Self::ThisDevice
                } else if !name.contains('.') || matches!(last, "local" | "lan") {
                    Self::PrivateNetwork
                } else {
                    Self::Hosted
                }
            }
            url::Host::Ipv4(address) if address.is_loopback() => Self::ThisDevice,
            url::Host::Ipv4(address) if address.is_private() || address.is_link_local() => {
                Self::PrivateNetwork
            }
            url::Host::Ipv6(address) if address.is_loopback() => Self::ThisDevice,
            url::Host::Ipv6(address)
                if address.is_unique_local() || address.is_unicast_link_local() =>
            {
                Self::PrivateNetwork
            }
            url::Host::Ipv4(_) | url::Host::Ipv6(_) => Self::Hosted,
        })
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

/// The namespace of a [`SecretReference`] that names an entry of the workspace's vault.
pub const WORKSPACE_VAULT: &str = "tilo.vault";

impl SecretReference {
    pub fn provider_api_key(profile_id: &str) -> Self {
        Self {
            vault: "tilo.secret-service".to_string(),
            locator: format!("agent-provider/{profile_id}/api-key"),
        }
    }

    /// The entry of the workspace's vault a provider's key is, by the entry's reference
    /// (`vault.item:<entry id>`): what a host that keeps a workspace's providers names, so the
    /// key lives in the workspace's one vault and is read from it at each call.
    pub fn workspace_vault(reference: &str) -> Self {
        Self {
            vault: WORKSPACE_VAULT.to_owned(),
            locator: reference.to_owned(),
        }
    }

    /// The reference of the workspace vault's entry this names, where it names one
    /// ([`Self::workspace_vault`]).
    #[must_use]
    pub fn workspace_vault_item(&self) -> Option<&str> {
        (self.vault == WORKSPACE_VAULT && self.is_valid()).then_some(self.locator.as_str())
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

/// Who a [`SignInCredential`] signs the person in with. Each decides how the
/// gateway turns the sealed grant into a request credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SignInIssuer {
    /// Google: Vertex AI, or the Gemini API, billed to a Cloud project.
    Google,
    /// The person's `OpenAI` account, for their `ChatGPT` subscription. Not an
    /// `OpenAI` API key: requests go to the `ChatGPT` backend
    /// ([`presets::CHATGPT_ENDPOINT`]) in the `OpenAI` Responses format.
    ChatGpt,
    /// GitHub, for a Copilot subscription: the gateway trades the GitHub
    /// grant for a short-lived Copilot token.
    GitHubCopilot,
    /// Hugging Face, for inference billed to the person's account.
    HuggingFace,
}

impl SignInIssuer {
    pub const ALL: [Self; 4] = [
        Self::Google,
        Self::ChatGpt,
        Self::GitHubCopilot,
        Self::HuggingFace,
    ];

    /// The stored issuer name.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Google => GOOGLE_SIGN_IN,
            Self::ChatGpt => "chatgpt",
            Self::GitHubCopilot => "github-copilot",
            Self::HuggingFace => "huggingface",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|issuer| issuer.id() == id)
    }

    /// Who the person signs in with.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::ChatGpt => "ChatGPT",
            Self::GitHubCopilot => "GitHub",
            Self::HuggingFace => "Hugging Face",
        }
    }
}

/// A sign-in held by account credentials in place of an API key: the gateway
/// mints a short-lived access token from it for each request and refreshes
/// it when due. The grant itself lives in this device's vault under
/// [`sign_in_grant_id`], derived from the profile id so a profile cannot
/// name another account's grant (Gmail's, say).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct SignInCredential {
    /// Who signed the person in: a [`SignInIssuer::id`]. Kept as text so a
    /// reader that does not know a newer issuer still reads the profile,
    /// and shows it as unsupported rather than losing the configuration.
    pub issuer: String,
    /// The Google Cloud project billed for requests made with a person's own
    /// credentials, sent as `x-goog-user-project`. Without it Google bills
    /// the project of Tilo's `OAuth` client, which the person cannot use.
    /// Google sign-ins only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_project: Option<String>,
    /// Where the grant is kept when it is not in this device's vault: an entry of the
    /// workspace's vault, as a host that keeps a workspace's providers names it
    /// ([`SecretReference::workspace_vault`]). Absent on a device, which keeps its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<SecretReference>,
}

settings_store::read_through_json_value!(SignInCredential);

impl SignInCredential {
    /// A sign-in with `issuer` and no billing project.
    pub fn new(issuer: SignInIssuer) -> Self {
        Self {
            issuer: issuer.id().to_string(),
            billing_project: None,
            kept: None,
        }
    }

    pub fn google(billing_project: impl Into<String>) -> Self {
        Self {
            billing_project: Some(billing_project.into()),
            ..Self::new(SignInIssuer::Google)
        }
    }

    /// The issuer, `None` for one this build does not know.
    pub fn issuer(&self) -> Option<SignInIssuer> {
        SignInIssuer::from_id(&self.issuer)
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
    /// The model's own declared output limit: the most tokens one answer may
    /// take, its reasoning included. `None` means its list said nothing, not
    /// unlimited; a caller then asks for
    /// [`ModelDescriptor::DEFAULT_OUTPUT_TOKENS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
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
    /// Whether the model calls tools. An agent plans by calling actions, so
    /// a model without it cannot be chosen for one
    /// ([`ProviderConfig::set_agent_model`]). Unknown reads as `true`: a
    /// model nothing says anything about is offered, and the provider's
    /// own refusal names it.
    pub tool_calling: bool,
    /// Who said what `vision` and `tool_calling` hold: the provider's model
    /// list, Tilo's table of model families, or the person. A person's
    /// answer is never replaced by a later list.
    #[serde(skip_serializing_if = "CapabilitySource::is_unknown")]
    pub capabilities_from: CapabilitySource,
}

settings_store::read_through_json_value!(ModelDescriptor);

/// Where a model's `vision` and `tool_calling` came from
/// ([`ModelDescriptor::capabilities_from`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum CapabilitySource {
    /// The provider's model list said so.
    Provider,
    /// Tilo's table of known model families ([`capabilities::family`]).
    Family,
    /// The person set them.
    Person,
    /// Nothing said: `vision` off, `tool_calling` on. Also what a source
    /// this build does not know reads as, so a newer writer's settings
    /// still read.
    #[default]
    #[serde(other)]
    Unknown,
}

settings_store::read_through_json_value!(CapabilitySource);

impl CapabilitySource {
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

impl Default for ModelDescriptor {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            context_window: None,
            max_output_tokens: None,
            request_limits: None,
            revision: None,
            vision: false,
            face_recognition: false,
            tool_calling: true,
            capabilities_from: CapabilitySource::Unknown,
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
    /// What one answer may take when nothing declared the model's limit: one
    /// whole turn of a task — a plan with full arguments, or an answer — and
    /// a reasoning model's thinking before it, which counts against the same
    /// bound. A metered endpoint reserves credit for all of it.
    pub const DEFAULT_OUTPUT_TOKENS: u32 = 16_384;

    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            label: id.clone(),
            id,
            ..Self::default()
        }
    }

    /// A hosted model named by hand: what its family is known to do
    /// ([`capabilities::family`]), else nothing known.
    pub fn named(id: impl Into<String>) -> Self {
        let mut model = Self::new(id);
        capabilities::apply_family(&mut model);
        model
    }

    /// Whether the person set this model's capabilities.
    pub const fn capabilities_set_by_person(&self) -> bool {
        matches!(self.capabilities_from, CapabilitySource::Person)
    }

    pub fn display_label(&self) -> &str {
        if self.label.trim().is_empty() {
            &self.id
        } else {
            &self.label
        }
    }

    /// The most tokens one answer may take: the model's own declared limit,
    /// else [`Self::DEFAULT_OUTPUT_TOKENS`]. Never more than its context
    /// leaves room for after its input, when both are known, so a small local
    /// context never makes a request the server must refuse.
    #[must_use]
    pub fn output_tokens(&self) -> u32 {
        let declared = self
            .max_output_tokens
            .unwrap_or(Self::DEFAULT_OUTPUT_TOKENS);
        self.context_window
            .map_or(declared, |context| declared.min(context))
    }

    /// How many characters of conversation this model reads at once, by what its descriptor
    /// knows of its context, about four characters a token; a model that says nothing reads a
    /// generous default. A turn keeps its conversation within this bound so that a small local
    /// window is never overflowed.
    #[must_use]
    pub fn context_chars(&self) -> usize {
        let tokens = self
            .request_limits
            .as_ref()
            .and_then(|limits| limits.runtime_context_tokens)
            .or(self.context_window)
            .unwrap_or(128_000);
        let headroom = tokens.saturating_sub(self.max_output_tokens.unwrap_or(8_192));
        usize::try_from(headroom)
            .unwrap_or(usize::MAX)
            .saturating_mul(3)
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
    /// The `OpenAI` API an OpenAI-compatible profile's agent turns take.
    /// `None` takes Responses at `OpenAI`'s own API, whose reasoning models
    /// call tools only there, and chat completions at any other endpoint
    /// ([`transform::openai_api`]). A `ChatGPT` sign-in always takes
    /// Responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_api: Option<OpenAiApi>,
    /// The header carrying the API key, when it is not the protocol's own
    /// (`Authorization: Bearer` for OpenAI-compatible, `x-api-key` for
    /// Anthropic): Azure's `api-key`, or a bearer token at an
    /// Anthropic-compatible endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_header: Option<KeyHeader>,
    /// Where the provider lists its models, when it is not the protocol's
    /// own ([`transform::models_url`]): relative to the endpoint, or from
    /// its origin when it starts with `/`. An empty path says the provider
    /// lists none: its models are typed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_path: Option<String>,
    /// Headers sent with every request to this provider beside its key: what
    /// a provider asks of each call, such as the organisation or workspace a
    /// key with access to several bills (`anthropic-workspace-id`,
    /// `openai-organization`). They are settings, shown wherever the provider
    /// is, never secrets: the key has its own place, and
    /// [`ProviderHeader::check`] refuses the headers the key and the protocol
    /// own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<ProviderHeader>,
}

settings_store::read_through_json_value!(ProviderProfile);

/// One header a provider is sent with every request ([`ProviderProfile::headers`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderHeader {
    pub name: String,
    pub value: String,
}

/// Headers a provider's own settings may not send: the ones its key travels in, the
/// protocol's version, and the ones the HTTP client writes itself.
const OWNED_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "api-key",
    "anthropic-version",
    "host",
    "content-type",
    "content-length",
    "transfer-encoding",
    "connection",
    "cookie",
];

impl ProviderHeader {
    /// Whether this header may be sent as a provider's setting.
    ///
    /// # Errors
    /// The words a person reads: a name that is no HTTP header name, one the key or the
    /// protocol sends, or a value no header can carry.
    pub fn check(&self) -> Result<(), String> {
        let name = self.name.trim();
        let token = |byte: u8| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte);
        if name.is_empty() || !name.bytes().all(token) {
            return Err(format!("{name:?} is not the name of a header"));
        }
        if OWNED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!(
                "{name} is sent by Tilo itself: the provider's key has its own setting"
            ));
        }
        if !self
            .value
            .bytes()
            .all(|byte| byte == b'\t' || (b' '..=b'~').contains(&byte))
        {
            return Err(format!("The value of {name} cannot be sent in a header"));
        }
        Ok(())
    }
}

/// The request header an API key travels in ([`ProviderProfile::key_header`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub enum KeyHeader {
    /// `Authorization: Bearer <key>`.
    #[serde(rename = "authorization")]
    Bearer,
    /// `x-api-key: <key>`, Anthropic's.
    #[serde(rename = "x-api-key")]
    XApiKey,
    /// `api-key: <key>`, Azure `OpenAI`'s.
    #[serde(rename = "api-key")]
    ApiKey,
}

settings_store::read_through_json_value!(KeyHeader);

impl KeyHeader {
    /// The header a `protocol` sends its key in unless told otherwise.
    pub const fn of(protocol: ProviderProtocol) -> Self {
        match protocol {
            ProviderProtocol::OpenAiCompatible => Self::Bearer,
            ProviderProtocol::Anthropic => Self::XApiKey,
        }
    }

    /// The header's name, as sent.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bearer => "authorization",
            Self::XApiKey => "x-api-key",
            Self::ApiKey => "api-key",
        }
    }
}

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

/// The request API of an OpenAI-compatible endpoint's agent turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum OpenAiApi {
    /// `POST /chat/completions`, which every compatible server speaks.
    ChatCompletions,
    /// `POST /responses`.
    Responses,
}

settings_store::read_through_json_value!(OpenAiApi);

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
            openai_api: None,
            key_header: None,
            models_path: None,
            headers: Vec::new(),
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
            openai_api: None,
            key_header: None,
            models_path: None,
            headers: Vec::new(),
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
            openai_api: None,
            key_header: None,
            models_path: None,
            headers: Vec::new(),
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
            openai_api: None,
            key_header: None,
            models_path: None,
            headers: Vec::new(),
        }
    }

    /// A Gemini API profile signed in with Google, billed to `project`.
    pub fn google_gemini(project: &str) -> Self {
        Self {
            sign_in: Some(SignInCredential::google(project)),
            credential: None,
            ..presets::Preset::GEMINI.profile()
        }
    }

    /// Whether this profile speaks to Google's own Gemini endpoint
    /// ([`presets::GEMINI_ENDPOINT`]), whatever path it names there.
    #[must_use]
    pub fn is_googles_endpoint(&self) -> bool {
        let googles = url::Url::parse(presets::GEMINI_ENDPOINT).ok();
        url::Url::parse(self.endpoint.trim()).is_ok_and(|url| {
            url.scheme() == "https"
                && url.domain().is_some()
                && url.domain() == googles.as_ref().and_then(url::Url::domain)
        })
    }

    /// Whether this profile signs in with Google instead of using a key.
    pub fn signs_in_with_google(&self) -> bool {
        self.sign_in_issuer() == Some(SignInIssuer::Google)
    }

    /// Who this profile signs in with, `None` for a key, no credential or
    /// an issuer this build does not know.
    pub fn sign_in_issuer(&self) -> Option<SignInIssuer> {
        self.sign_in.as_ref().and_then(SignInCredential::issuer)
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

/// Whether `profile` is a model runner the gateway supervises on this device:
/// OpenAI-compatible and reachable only on loopback. Work that must stay on
/// the device (its image operations, private history prose) admits nothing
/// else, and apps discovering such a model apply this same rule so they never
/// offer one the gateway would refuse.
#[must_use]
pub fn is_supervised_loopback(profile: &ProviderProfile) -> bool {
    profile.location == EndpointLocation::ThisDevice
        && profile.protocol == ProviderProtocol::OpenAiCompatible
        && loopback_http_endpoint(&profile.endpoint)
}

fn loopback_http_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") || url.username() != "" || url.password().is_some()
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

/// Whether `endpoint` names a literal loopback address on this device: an
/// `http`/`https` URL whose host is `127.0.0.1` or `::1`, with no userinfo.
/// A name, even `localhost`, is resolved by something outside the device's
/// own network stack, so it does not qualify.
#[must_use]
pub fn literal_loopback_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") || url.username() != "" || url.password().is_some()
    {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    }
}

/// Whether `profile` is an endpoint on this device that authenticates no
/// one: no credential and no sign-in, classified as this device, at a
/// literal loopback address. The model gateway resolves a key for every
/// other profile and refuses a provider without one, and the request
/// headers send none for this one. One rule decides both, so a keyless
/// local plan and the request it sends cannot disagree.
#[must_use]
pub fn is_unauthenticated_local(profile: &ProviderProfile) -> bool {
    profile.location == EndpointLocation::ThisDevice
        && profile.credential.is_none()
        && profile.sign_in.is_none()
        && literal_loopback_endpoint(&profile.endpoint)
}

/// The hosted providers of a configuration, as they roam with the person:
/// each names an endpoint on the internet and a reference to its key, never
/// the key, which each device keeps in its own vault.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct HostedProfiles {
    pub profiles: Vec<ProviderProfile>,
}

settings_store::read_through_json_value!(HostedProfiles);

/// A portable profile list, the user's deliberate default and the models
/// they chose for particular agents and for each kind of interaction. Kept
/// separate from a request: a request records the model it was asked of, so
/// changing these does not change work already in progress.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct ProviderConfig {
    pub profiles: Vec<ProviderProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    /// The model an agent plans with in place of the default, by agent id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_models: BTreeMap<String, AgentModel>,
    /// The model one kind of interaction runs on, by [`Interaction::id`]. A
    /// kind with no choice follows the default, or the agent's own model.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub interaction_models: BTreeMap<String, AgentModel>,
}

settings_store::read_through_json_value!(ProviderConfig);

/// A model of one profile, chosen for an agent or for a kind of interaction.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct AgentModel {
    pub profile: String,
    pub model: String,
}

settings_store::read_through_json_value!(AgentModel);

#[cfg(feature = "model")]
tilo_resource::json_model!(AgentModel);

/// What a person is asking of a model, as far as choosing a model goes.
///
/// One kind is one exchange with a different appetite: a translation or a
/// dictated sentence is one short answer, a task runs for a plan and many
/// steps. Each kind carries its own default model, and a request may name
/// another one for itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum Interaction {
    /// One short answer computed from an input: translate, summarise,
    /// extract, classify, rewrite.
    OneShot,
    /// An instruction worked through the actions a person allows.
    Task,
    /// A part of a task.
    Subtask,
    /// A person's message to their agent, answered rather than acted on.
    Request,
    /// One label chosen for an input.
    Classifier,
    /// Tidying text a person dictated.
    Dictation,
}

settings_store::read_through_json_value!(Interaction);

impl Interaction {
    /// Every kind, in the order Settings lists them.
    pub const ALL: &'static [Self] = &[
        Self::OneShot,
        Self::Task,
        Self::Subtask,
        Self::Request,
        Self::Classifier,
        Self::Dictation,
    ];

    /// This kind's stable id, which is the key its choice is stored under.
    pub const fn id(self) -> &'static str {
        match self {
            Self::OneShot => "one_shot",
            Self::Task => "task",
            Self::Subtask => "subtask",
            Self::Request => "request",
            Self::Classifier => "classifier",
            Self::Dictation => "dictation",
        }
    }

    /// The kind stored under `id`, if this build knows it.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.id() == id)
    }
}

impl std::fmt::Display for Interaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

impl ProviderConfig {
    /// The model and where it runs, chosen for `kind`; `None` while nothing
    /// is chosen for it, or the choice names a provider that is switched off
    /// or no longer offers that model.
    #[must_use]
    pub fn interaction_selection(
        &self,
        kind: Interaction,
    ) -> Option<(&ProviderProfile, &ModelDescriptor)> {
        let chosen = self.interaction_models.get(kind.id())?;
        let profile = self
            .profiles
            .iter()
            .find(|profile| profile.id == chosen.profile && profile.enabled)?;
        let model = profile.models.iter().find(|m| m.id == chosen.model)?;
        Some((profile, model))
    }

    /// Choose `chosen` of a profile for `kind`, or with `None` let that kind
    /// follow the default again.
    pub fn set_interaction_model(
        &mut self,
        kind: Interaction,
        chosen: Option<AgentModel>,
    ) -> Result<(), &'static str> {
        let Some(chosen) = chosen else {
            self.interaction_models.remove(kind.id());
            return Ok(());
        };
        if !self.profiles.iter().any(|profile| {
            profile.id == chosen.profile && profile.models.iter().any(|m| m.id == chosen.model)
        }) {
            return Err("That provider does not offer this model.");
        }
        self.interaction_models.insert(kind.id().to_owned(), chosen);
        Ok(())
    }
}

impl ProviderConfig {
    pub fn load() -> Self {
        Self::from_document(&settings_store::load())
    }

    /// The configuration in `source`: another profile's settings.
    pub fn load_in(source: &dyn settings_store::SettingsSource) -> Self {
        Self::from_document(&source.load())
    }

    /// The configuration `document` holds: this device's providers and the
    /// person's hosted ones.
    pub(crate) fn from_document(document: &Map<String, Value>) -> Self {
        let leaf = |section: &str, key: &str| {
            document
                .get(section)
                .and_then(|section| section.get(key))
                .cloned()
        };
        let local = leaf(SETTINGS_SECTION, CONFIG_KEY)
            .and_then(|value| serde_json::from_value::<Self>(value).ok())
            .unwrap_or_default();
        let hosted = leaf(HOSTED_SECTION, HOSTED_KEY)
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
                agent_models: self.agent_models,
                interaction_models: self.interaction_models,
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
            agent_models: local.agent_models,
            interaction_models: local.interaction_models,
        }
    }

    /// This configuration as its two sections store it: this device's
    /// ([`SETTINGS_SECTION`]) and the person's ([`HOSTED_SECTION`]).
    fn sections(&self) -> std::io::Result<(Map<String, Value>, Map<String, Value>)> {
        let (local, hosted) = self.clone().sanitised().stored();
        let leaf = |key: &str, value: Value| Map::from_iter([(key.to_string(), value)]);
        Ok((
            leaf(
                CONFIG_KEY,
                serde_json::to_value(local).map_err(std::io::Error::other)?,
            ),
            leaf(
                HOSTED_KEY,
                serde_json::to_value(hosted).map_err(std::io::Error::other)?,
            ),
        ))
    }

    /// Merge our single configuration payload into the shared settings file.
    /// Other Settings panes retain their sections, and no secret can enter the
    /// document because `ProviderProfile` exposes a reference only.
    pub fn save(&self) -> std::io::Result<()> {
        let source = settings_store::source()
            .ok_or_else(|| std::io::Error::other("no settings in this process"))?;
        self.save_in(source.as_ref())
    }

    /// Save this configuration to `source`: another profile's settings.
    pub fn save_in(&self, source: &dyn settings_store::SettingsSource) -> std::io::Result<()> {
        let (local, hosted) = self.sections()?;
        source.update_sections(
            vec![
                (SETTINGS_SECTION.to_owned(), local),
                (HOSTED_SECTION.to_owned(), hosted),
            ],
            &settings_store::ChangeOrigin::default(),
        )
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
            }
            endpoint_issues(profile, &mut issues);
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
        for (agent, chosen) in &self.agent_models {
            if agent.trim().is_empty() || self.offers_nothing(chosen) {
                issues.push(ProfileIssue::new(
                    &chosen.profile,
                    "An agent's model is not offered by its provider.",
                ));
            }
        }
        self.choice_issues(&mut issues);
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

    /// The profile and model `agent` plans with: the one chosen for it, else
    /// the default profile's default model. `None` while the choice names a
    /// provider that is switched off, or nothing is chosen at all: a caller
    /// asks rather than guessing another model.
    pub fn selection_for(&self, agent: &str) -> Option<(&ProviderProfile, &ModelDescriptor)> {
        let (profile, model) = if let Some(chosen) = self.agent_models.get(agent) {
            let profile = self
                .profiles
                .iter()
                .find(|profile| profile.id == chosen.profile && profile.enabled)?;
            (profile, &chosen.model)
        } else {
            let profile = self.active_profile()?;
            (profile, profile.default_model.as_ref()?)
        };
        Some((profile, profile.models.iter().find(|m| &m.id == model)?))
    }

    /// Choose `model` of `profile` for `agent`, or with `None` let it follow
    /// the default again.
    ///
    /// # Errors
    /// When `agent` is empty, `profile` does not offer `model`, or the model
    /// does not call tools ([`NO_TOOL_CALLING`]).
    pub fn set_agent_model(
        &mut self,
        agent: &str,
        chosen: Option<AgentModel>,
    ) -> Result<(), &'static str> {
        let Some(chosen) = chosen else {
            self.agent_models.remove(agent);
            return Ok(());
        };
        if agent.trim().is_empty() {
            return Err("Name the agent.");
        }
        let model = self
            .profiles
            .iter()
            .filter(|profile| profile.id == chosen.profile)
            .find_map(|profile| profile.models.iter().find(|m| m.id == chosen.model))
            .ok_or("That provider does not offer this model.")?;
        if !model.tool_calling {
            return Err(NO_TOOL_CALLING);
        }
        self.agent_models.insert(agent.to_owned(), chosen);
        Ok(())
    }

    /// The agents that plan with `model` of `profile`.
    pub fn agents_planning_with<'a>(
        &'a self,
        profile: &'a str,
        model: &'a str,
    ) -> impl Iterator<Item = &'a str> + 'a {
        self.agent_models
            .iter()
            .filter(move |(_, chosen)| chosen.profile == profile && chosen.model == model)
            .map(|(agent, _)| agent.as_str())
    }

    /// Whether no profile offers the model `chosen` names.
    fn offers_nothing(&self, chosen: &AgentModel) -> bool {
        !self.profiles.iter().any(|profile| {
            profile.id == chosen.profile && profile.models.iter().any(|m| m.id == chosen.model)
        })
    }

    /// Diagnose the models chosen for kinds of interaction: one this build
    /// does not know, or one no profile offers.
    fn choice_issues(&self, issues: &mut Vec<ProfileIssue>) {
        for (kind, chosen) in &self.interaction_models {
            if Interaction::from_id(kind).is_none() || self.offers_nothing(chosen) {
                issues.push(ProfileIssue::new(
                    &chosen.profile,
                    "A default model for a kind of interaction is not offered here.",
                ));
            }
        }
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

    /// The profile `id` as it may change, or [`ProviderChoiceError::ProviderGone`]
    /// when nothing has its id. The one way a caller reaches a profile to
    /// change it, so the pane and a service change it the same way.
    ///
    /// # Errors
    /// [`ProviderChoiceError::ProviderGone`] when no profile has the id.
    pub fn profile_mut(&mut self, id: &str) -> Result<&mut ProviderProfile, ProviderChoiceError> {
        self.profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or(ProviderChoiceError::ProviderGone)
    }

    /// Make `model` the default of `provider`, which offers it afterwards
    /// even if it was written by hand rather than listed by the provider.
    /// `listed` is what the provider's list says of it; without one, a model
    /// the profile does not have yet gets what its family is known to do
    /// ([`ModelDescriptor::named`]). The one choice the pane's "Use model"
    /// and the canvas composer's switcher make.
    ///
    /// # Errors
    /// [`ProviderChoiceError::NotAModel`] when `model` is blank, longer than
    /// 256 bytes, or holds a control character, and
    /// [`ProviderChoiceError::ProviderGone`] when no profile has the
    /// provider's id.
    pub fn take_model(
        &mut self,
        provider: &str,
        model: &str,
        listed: Option<ModelDescriptor>,
    ) -> Result<(), ProviderChoiceError> {
        let model = model.trim();
        if model.is_empty() || model.len() > 256 || model.contains(char::is_control) {
            return Err(ProviderChoiceError::NotAModel);
        }
        let profile = self.profile_mut(provider)?;
        match profile.models.iter_mut().find(|known| known.id == model) {
            // What the provider now says about a model the person already
            // has; their own answer stays.
            Some(known) => {
                if let Some(listed) = &listed {
                    capabilities::refresh(known, listed);
                }
            }
            None => profile
                .models
                .push(listed.unwrap_or_else(|| ModelDescriptor::named(model.to_owned()))),
        }
        profile.default_model = Some(model.to_owned());
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Option<ProviderProfile> {
        let index = self.profiles.iter().position(|profile| profile.id == id)?;
        let removed = self.profiles.remove(index);
        if self.default_profile.as_deref() == Some(id) {
            self.default_profile = None;
        }
        self.agent_models.retain(|_, chosen| chosen.profile != id);
        self.interaction_models
            .retain(|_, chosen| chosen.profile != id);
        Some(removed)
    }

    /// The configuration as it is stored and read: profiles without an id,
    /// models without an id, and defaults and agents' models naming no model
    /// are dropped.
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
        let unknown = |chosen: &AgentModel| {
            !self.profiles.iter().any(|profile| {
                profile.id == chosen.profile && profile.models.iter().any(|m| m.id == chosen.model)
            })
        };
        self.agent_models.retain(|_, chosen| !unknown(chosen));
        self.interaction_models
            .retain(|kind, chosen| Interaction::from_id(kind).is_some() && !unknown(chosen));
        self
    }
}

/// What is wrong with `profile`'s endpoint beyond its absence: no TLS to a
/// hosted provider.
fn endpoint_issues(profile: &ProviderProfile, issues: &mut Vec<ProfileIssue>) {
    if profile.endpoint.trim().is_empty() {
        return;
    }
    if !is_safe_endpoint(&profile.endpoint, profile.location) {
        issues.push(ProfileIssue::new(
            &profile.id,
            "Endpoint must be HTTPS unless it is a local or private endpoint.",
        ));
    }
}

/// Why a model cannot plan for an agent: an agent acts by calling tools.
pub const NO_TOOL_CALLING: &str =
    "This model does not call tools, so an agent cannot plan with it.";

/// Why a person's choice of a provider or a model was refused, as
/// [`ProviderConfig::take_model`] answers it: each carries no words, leaving
/// the words to whoever shows them (the pane's localized keys, the canvas
/// composer's own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderChoiceError {
    /// The model is blank, longer than 256 bytes, or holds a control
    /// character.
    NotAModel,
    /// No profile has the provider's id.
    ProviderGone,
}

impl std::fmt::Display for ProviderChoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAModel => "Enter a model ID on one line.",
            Self::ProviderGone => "Provider is no longer available.",
        })
    }
}

impl std::error::Error for ProviderChoiceError {}

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
        if sign_in.issuer().is_none() {
            issues.push(ProfileIssue::new(
                &profile.id,
                "Profile signs in with an unsupported provider.",
            ));
        }
        if sign_in.billing_project.is_some() && sign_in.issuer() != Some(SignInIssuer::Google) {
            issues.push(ProfileIssue::new(
                &profile.id,
                "Only a Google sign-in names a billing project.",
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

/// Typed local-speech configuration and its private worker wire format.
pub mod speech;
