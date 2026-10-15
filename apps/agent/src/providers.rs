//! Agent-runner view of Settings' provider configuration.
//!
//! A task captures this selection before it begins. That prevents a Settings
//! change from silently moving work already under way to a different provider,
//! and leaves secret resolution to the desktop broker rather than environment
//! variables or the replicated task state.

use agent_provider_config::{ModelDescriptor, ProviderConfig, ProviderProfile, SecretReference};

/// The non-secret endpoint selection captured by a new task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSelection {
    pub profile_id: String,
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

    /// A task's provider selection names a model and never a secret, and the
    /// packaged agent has no route of its own to a provider or an
    /// environment key: the desktop-owned gateway is the only way out.
    #[test]
    fn a_provider_is_reached_only_through_the_gateway_with_an_explicit_model_and_no_secret() {
        // Case `task_selection_captures_a_model_but_not_a_secret`.
        {
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

        // Case `incomplete_provider_requires_an_explicit_model_choice`.
        {
            let profile = ProviderProfile::local_vllm();
            let config = ProviderConfig {
                default_profile: Some(profile.id.clone()),
                profiles: vec![profile],
            };
            assert!(ProviderSelection::from_config(&config).is_none());
        }

        // Case `packaged_agent_has_no_direct_provider_or_environment_key_route`,
        // moved here from the one-test `tests/hosted_gateway_boundary.rs`.
        {
            let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src/");
            let model = std::fs::read_to_string(format!("{root}model.rs")).unwrap();
            let providers = std::fs::read_to_string(format!("{root}providers.rs")).unwrap();
            assert!(model.contains("tilo_model_gateway::complete_streaming"));
            assert!(model.contains("ModelConversation::new"));
            assert!(!model.contains("ANTHROPIC_API_KEY"));
            assert!(!model.contains("reqwest::"));
            // This scan now reads the file it is written in, so the needle is
            // assembled at run time: spelling it out here would find itself.
            let secret_resolver = ["Secret", "Resolver"].concat();
            assert!(!providers.contains(&secret_resolver));
        }
    }
}
