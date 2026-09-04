//! Provider-neutral configuration for qOS agents.
//!
//! This crate deliberately stores *descriptions* of model providers, never a
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
/// vLLM server must never look like a hosted API merely because it shares the
/// OpenAI wire protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointLocation {
    /// A provider-operated internet endpoint.
    #[default]
    Hosted,
    /// A loopback endpoint on this qOS device, e.g. vLLM or Ollama.
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
/// `agent-provider/01J.../api-key`. It is intentionally not an environment
/// variable name: the desktop broker can apply per-agent access policy before
/// handing the secret to an outbound request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretReference {
    /// The broker/vault namespace, usually `qos.secret-service`.
    pub vault: String,
    /// Opaque item locator within `vault`; never the secret value.
    pub locator: String,
}

impl SecretReference {
    pub fn provider_api_key(profile_id: &str) -> Self {
        Self {
            vault: "qos.secret-service".to_string(),
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
    pub vision: bool,
    pub tool_calling: bool,
}

impl Default for ModelDescriptor {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            context_window: None,
            vision: false,
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
    /// Absent for an intentionally unauthenticated local endpoint. The secret
    /// itself is never accepted or serialised by this type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<SecretReference>,
    pub enabled: bool,
    pub models: Vec<ModelDescriptor>,
    /// The default model id within `models`. Missing/unknown is represented
    /// by `None`, so callers prompt instead of silently choosing a model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

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
        }
    }

    /// An OpenAI-compatible local profile suitable for vLLM. It is deliberately
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
        Self::from_section(section)
    }

    pub fn load_at(path: &Path) -> Self {
        Self::from_section(
            settings_store::load_at(path)
                .remove(SETTINGS_SECTION)
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default(),
        )
    }

    fn from_section(section: Map<String, Value>) -> Self {
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
    pub fn save(&self) -> std::io::Result<()> {
        let config = self.clone().sanitised();
        let mut section = Map::new();
        section.insert(
            CONFIG_KEY.to_string(),
            serde_json::to_value(config).map_err(std::io::Error::other)?,
        );
        settings_store::update_section(SETTINGS_SECTION, section)
    }

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
    fn local_vllm_is_openai_compatible_and_plainly_local() {
        let profile = ProviderProfile::local_vllm();
        assert_eq!(profile.protocol, ProviderProtocol::OpenAiCompatible);
        assert_eq!(profile.location, EndpointLocation::ThisDevice);
        assert!(profile.endpoint.starts_with("http://127.0.0.1"));
        assert!(profile.is_local_vllm_candidate());
        assert!(profile.credential.is_none());
    }

    #[test]
    fn hosted_profiles_hold_only_a_vault_reference() {
        let profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        let json = serde_json::to_string(&profile).unwrap();
        assert!(json.contains("qos.secret-service"));
        assert!(json.contains("agent-provider/"));
        assert!(!json.contains("sk-ant-"));
        assert!(!json.contains("api_key\":\""));
    }

    #[test]
    fn hosted_http_endpoint_is_rejected_but_loopback_http_is_not() {
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

    #[test]
    fn bad_default_is_not_active() {
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
