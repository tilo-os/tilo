//! Agent-runner view of Settings' provider configuration.
//!
//! A task captures this selection before it begins. That prevents a Settings
//! change from silently moving work already under way to a different provider,
//! and leaves secret resolution to the desktop broker rather than environment
//! variables or the replicated task state.

use agent_provider_config::{ModelDescriptor, ProviderConfig, ProviderProfile, SecretReference};
use zeroize::Zeroizing;

/// A secret resolved just-in-time by a desktop-owned credential broker.
/// It deliberately has no `Debug` implementation, so request tracing cannot
/// accidentally render the bearer value.
pub struct ResolvedSecret(Zeroizing<String>);

impl ResolvedSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

/// Boundary between the agent process and the desktop secret service. The
/// resolver receives only a non-secret [`SecretReference`], so profile
/// storage, Settings, and task records never have to carry a raw API key.
pub trait SecretResolver: Send + Sync {
    fn resolve(&self, reference: &SecretReference) -> Result<ResolvedSecret, String>;
}

/// A safe default for contexts that do not have the secret portal wired yet.
/// It fails closed instead of falling back to environment variables.
pub struct UnavailableSecretResolver;

impl SecretResolver for UnavailableSecretResolver {
    fn resolve(&self, _reference: &SecretReference) -> Result<ResolvedSecret, String> {
        Err("the desktop credential broker is unavailable".to_string())
    }
}

/// The non-secret endpoint selection captured by a new task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSelection {
    pub profile_id: String,
    pub profile_label: String,
    pub endpoint: String,
    pub protocol: agent_provider_config::ProviderProtocol,
    pub model: ModelDescriptor,
    pub credential: Option<SecretReference>,
}

impl ProviderSelection {
    /// Resolve the user's currently enabled default. A profile without a
    /// declared default model is intentionally not guessed: the task center
    /// must ask the user/model-discovery flow to choose one.
    pub fn from_config(config: &ProviderConfig) -> Option<Self> {
        let profile = config.active_profile()?;
        let model_id = profile.default_model.as_ref()?;
        let model = profile
            .models
            .iter()
            .find(|model| &model.id == model_id)?
            .clone();
        Some(Self::from_profile(profile, model))
    }

    pub fn from_profile(profile: &ProviderProfile, model: ModelDescriptor) -> Self {
        Self {
            profile_id: profile.id.clone(),
            profile_label: profile.display_label().to_string(),
            endpoint: profile.endpoint.clone(),
            protocol: profile.protocol,
            model,
            credential: profile.credential.clone(),
        }
    }
}

/// Load the current Settings selection for a *new* task. Existing tasks use
/// their recorded [`ProviderSelection`] instead.
pub fn default_selection() -> Option<ProviderSelection> {
    ProviderSelection::from_config(&ProviderConfig::load())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_provider_config::{ProviderProfile, ProviderProtocol};

    #[test]
    fn task_selection_captures_a_model_but_not_a_secret() {
        let mut profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        profile.models.push(ModelDescriptor::new("claude-test"));
        profile.default_model = Some("claude-test".into());
        let config = ProviderConfig {
            default_profile: Some(profile.id.clone()),
            profiles: vec![profile],
        };
        let selected = ProviderSelection::from_config(&config).expect("configured provider");
        assert_eq!(selected.model.id, "claude-test");
        assert!(selected.credential.is_some());
        let serialised = format!("{selected:?}");
        assert!(!serialised.contains("sk-ant-"));
    }

    #[test]
    fn incomplete_provider_requires_an_explicit_model_choice() {
        let profile = ProviderProfile::local_vllm();
        let config = ProviderConfig {
            default_profile: Some(profile.id.clone()),
            profiles: vec![profile],
        };
        assert!(ProviderSelection::from_config(&config).is_none());
    }

    struct TestResolver;

    impl SecretResolver for TestResolver {
        fn resolve(&self, reference: &SecretReference) -> Result<ResolvedSecret, String> {
            assert!(reference.locator.starts_with("agent-provider/"));
            Ok(ResolvedSecret::new("test-only-secret"))
        }
    }

    #[test]
    fn resolver_receives_only_the_vault_reference() {
        let profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        let reference = profile.credential.as_ref().expect("hosted reference");
        let resolver = TestResolver;
        let secret = resolver.resolve(reference).expect("resolved test secret");
        assert_eq!(secret.expose(), "test-only-secret");
        let debug = format!("{reference:?}");
        assert!(!debug.contains("test-only-secret"));
    }
}
