//! Provider-neutral configuration for Tilo agents.
//!
//! This crate stores *descriptions* of model providers, never a
//! bearer token. A profile can point at a system-vault item via
//! [`SecretReference`], while the process that owns the secret service is
//! responsible for resolving that reference at call time. Keeping this
//! boundary here makes the same profile safe to render in Settings, sync as
//! non-secret device intent, or hand to a sandboxed agent.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod local;

/// The `settings.json` section owned by this crate.
pub const SETTINGS_SECTION: &str = "agent_providers";
const CONFIG_KEY: &str = "config";

/// The request dialect spoken by a provider endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    /// `/v1/chat/completions` and related OpenAI-compatible endpoints.
    #[default]
    OpenAiCompatible,
    /// Anthropic's `/v1/messages` API.
    Anthropic,
}

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

/// One model advertised by a profile. A model list is metadata, not a
/// capability grant: individual agent tasks still require their own action
/// permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelDescriptor {
    /// Provider-native model id sent over the wire.
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

impl Default for ModelDescriptor {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            context_window: None,
            revision: None,
            vision: false,
            face_recognition: false,
            tool_calling: true,
        }
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
#[serde(default)]
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
            enabled: true,
            models: Vec::new(),
            default_model: None,
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

    pub fn is_local_vllm_candidate(&self) -> bool {
        self.protocol == ProviderProtocol::OpenAiCompatible
            && self.location == EndpointLocation::ThisDevice
            && (self.endpoint.contains(":8000") || self.label.to_ascii_lowercase().contains("vllm"))
    }
}

/// A portable profile list and the user's deliberate default. Kept separate
/// from a task: tasks capture a chosen model at start so changing this default
/// does not change work already in progress.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub profiles: Vec<ProviderProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
}

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
        section
            .get(CONFIG_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value::<Self>(value).ok())
            .unwrap_or_default()
            .sanitised()
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
        let config = self.clone().sanitised();
        let mut section = Map::new();
        section.insert(
            CONFIG_KEY.to_string(),
            serde_json::to_value(config).map_err(std::io::Error::other)?,
        );
        settings_store::update_section(SETTINGS_SECTION, section)
    }

    /// Save this configuration to the shared settings document at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be serialised or the
    /// settings document at `path` cannot be updated.
    pub fn save_at(&self, path: &Path) -> std::io::Result<()> {
        let config = self.clone().sanitised();
        let mut section = Map::new();
        section.insert(
            CONFIG_KEY.to_string(),
            serde_json::to_value(config).map_err(std::io::Error::other)?,
        );
        settings_store::update_section_at(path, SETTINGS_SECTION, section)
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
}
