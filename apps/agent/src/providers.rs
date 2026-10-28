//! Agent-runner view of Settings' provider configuration.
//!
//! A task captures this selection before it begins. That prevents a Settings
//! change from silently moving work already under way to a different provider,
//! and leaves secret resolution to the desktop broker rather than environment
//! variables or the replicated task state.

use agent_provider_config::{
    Interaction, ModelDescriptor, ProviderConfig, ProviderProfile, SecretReference,
};

/// The non-secret endpoint selection captured by a new task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSelection {
    pub profile_id: String,
    /// The provider as its profile names it, for a run's own words about who
    /// planned it. Never a credential or an endpoint.
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
            profile_label: if profile.label.is_empty() {
                profile.id.clone()
            } else {
                profile.label.clone()
            },
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

/// Load the model Settings chose for `agent`, else the default, for a *new*
/// task.
pub fn agent_selection(agent: &str) -> Option<ProviderSelection> {
    let config = ProviderConfig::load();
    let (profile, model) = config.selection_for(agent)?;
    Some(ProviderSelection::from_profile(profile, model.clone()))
}

/// Load the model Settings chose for one kind of interaction, for a request
/// of that kind; `None` while that kind follows the default.
pub fn interaction_selection(kind: Interaction) -> Option<ProviderSelection> {
    let config = ProviderConfig::load();
    let (profile, model) = config.interaction_selection(kind)?;
    Some(ProviderSelection::from_profile(profile, model.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_provider_config::{AgentModel, ProviderProfile, ProviderProtocol};

    /// A request carries the kind of interaction it is, so a translation can
    /// run on the model chosen for short answers while a task runs on the
    /// larger one chosen for tasks.
    #[test]
    fn a_request_of_each_kind_is_asked_of_the_model_chosen_for_that_kind() {
        let mut small = ProviderProfile::local_vllm();
        small.models = vec![ModelDescriptor::new("small")];
        let mut large = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        large.models = vec![ModelDescriptor::new("large")];
        large.default_model = Some("large".into());
        let mut config = ProviderConfig {
            default_profile: Some(large.id.clone()),
            profiles: vec![small.clone(), large.clone()],
            ..Default::default()
        };
        config
            .set_interaction_model(
                Interaction::OneShot,
                Some(AgentModel {
                    profile: small.id,
                    model: "small".into(),
                }),
            )
            .unwrap();
        config
            .set_interaction_model(
                Interaction::Task,
                Some(AgentModel {
                    profile: large.id,
                    model: "large".into(),
                }),
            )
            .unwrap();
        let chosen = |kind| {
            config
                .interaction_selection(kind)
                .map(|(profile, model)| (profile.location, model.id.clone()))
        };
        assert_eq!(
            chosen(Interaction::OneShot),
            Some((
                agent_provider_config::EndpointLocation::ThisDevice,
                "small".into()
            ))
        );
        assert_eq!(
            chosen(Interaction::Task),
            Some((
                agent_provider_config::EndpointLocation::Hosted,
                "large".into()
            ))
        );
        assert_eq!(chosen(Interaction::Subtask), None, "it follows the task");
    }

    /// A task's provider selection names a model and never a secret, and the
    /// packaged agent has no route of its own to a provider or an
    /// environment key: the desktop-owned gateway is the only way out.
    #[test]
    fn a_provider_is_reached_only_through_the_gateway_with_an_explicit_model_and_no_secret() {
        {
            let mut profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
            profile.models.push(ModelDescriptor::new("claude-test"));
            profile.default_model = Some("claude-test".into());
            let config = ProviderConfig {
                default_profile: Some(profile.id.clone()),
                profiles: vec![profile],
                ..Default::default()
            };
            let selected = ProviderSelection::from_config(&config).expect("configured provider");
            assert_eq!(selected.model.id, "claude-test");
            assert!(selected.credential.is_some());
            let serialised = format!("{selected:?}");
            assert!(!serialised.contains("sk-ant-"));
        }

        {
            let profile = ProviderProfile::local_vllm();
            let config = ProviderConfig {
                default_profile: Some(profile.id.clone()),
                profiles: vec![profile],
                ..Default::default()
            };
            assert!(ProviderSelection::from_config(&config).is_none());
        }

        {
            let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src/");
            let model = std::fs::read_to_string(format!("{root}model.rs")).unwrap();
            let providers = std::fs::read_to_string(format!("{root}providers.rs")).unwrap();
            assert!(model.contains("tilo_model_client::complete_streaming"));
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
