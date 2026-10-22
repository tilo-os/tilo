//! The model a workspace agent is started with.

/// The model a workspace agent plans with, as the nonsecret selection the
/// agent accepts: profile, model and protocol only. The model chosen for
/// this agent, else the desktop's default. The agent cannot read Settings
/// from its sandbox, so without this it has no model and plans by keyword
/// alone; the model gateway keeps the endpoint and the credential.
#[cfg(target_os = "linux")]
pub(super) fn workspace_agent_provider(agent_id: &str) -> Option<String> {
    provider_selection(&agent_provider_config::ProviderConfig::load(), agent_id)
}

/// `agent_id`'s model in `config` as the agent's `--provider` selection, or
/// `None` while it has no model to plan with.
fn provider_selection(
    config: &agent_provider_config::ProviderConfig,
    agent_id: &str,
) -> Option<String> {
    let (profile, model) = config.selection_for(agent_id)?;
    let mut selection = serde_json::json!({
        "profile_id": profile.id,
        "model_id": model.id,
        "protocol": profile.protocol,
    });
    // Said only of a model that sees, so an agent shows it pictures.
    if model.vision
        && let Some(fields) = selection.as_object_mut()
    {
        fields.insert("vision".into(), true.into());
    }
    serde_json::to_string(&selection).ok()
}

#[cfg(test)]
mod tests {
    use super::provider_selection;

    /// A launched workspace agent cannot read Settings from its sandbox, so
    /// the desktop hands it its model, the one chosen for it or else the
    /// default, as exactly the nonsecret selection the agent accepts, and
    /// nothing while no model is chosen.
    #[test]
    fn a_workspace_agent_is_handed_its_model_selection_only() {
        use agent_provider_config::{AgentModel, ModelDescriptor, ProviderConfig, ProviderProfile};
        let mut profile =
            ProviderProfile::hosted(agent_provider_config::ProviderProtocol::OpenAiCompatible);
        profile.models = vec![
            ModelDescriptor::new("google/gemini-4-argon"),
            ModelDescriptor::new("small"),
        ];
        let mut config = ProviderConfig {
            default_profile: Some(profile.id.clone()),
            profiles: vec![profile.clone()],
            ..ProviderConfig::default()
        };
        assert_eq!(provider_selection(&config, "writer"), None);
        config.profiles[0].default_model = Some("google/gemini-4-argon".into());
        let selection = |config: &ProviderConfig, agent| -> serde_json::Value {
            serde_json::from_str(&provider_selection(config, agent).expect("a selection")).unwrap()
        };
        let expected = |model: &str| {
            serde_json::json!({
                "profile_id": profile.id,
                "model_id": model,
                "protocol": profile.protocol,
            })
        };
        assert_eq!(
            selection(&config, "writer"),
            expected("google/gemini-4-argon")
        );
        config
            .set_agent_model(
                "writer",
                Some(AgentModel {
                    profile: profile.id.clone(),
                    model: "small".into(),
                }),
            )
            .unwrap();
        assert_eq!(selection(&config, "writer"), expected("small"));
        assert_eq!(
            selection(&config, "reader"),
            expected("google/gemini-4-argon")
        );
        config.profiles[0].models[1].vision = true;
        let mut seeing = expected("small");
        seeing["vision"] = true.into();
        assert_eq!(selection(&config, "writer"), seeing);
    }
}
