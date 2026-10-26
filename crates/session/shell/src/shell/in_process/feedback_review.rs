//! The model that looks over a report before it is sent: the one the
//! person's AI settings choose for the report composer, else their default
//! model, asked through the model gateway as the composer, which may ask a
//! completion without tools and nothing more.

use agent_provider_config::ProviderConfig;

/// The model the review asks, and what the review calls it ("Anthropic
/// claude-haiku-5-5"); `None` while settings choose none or no gateway
/// runs in this session.
pub(super) fn review_model() -> Option<(ui::models::Models, String)> {
    let config = ProviderConfig::load();
    let (profile_id, model_id, name) = selection(&config)?;
    let gateway = gateway()?;
    match ui::models::Models::from_descriptor(
        std::os::fd::AsFd::as_fd(&gateway),
        profile_id,
        model_id,
    ) {
        Ok(models) => Some((models, name)),
        Err(error) => {
            tracing::debug!(%error, "reports are reviewed without a model");
            None
        }
    }
}

/// The profile and model the composer asks, and what the review calls it.
fn selection(config: &ProviderConfig) -> Option<(String, String, String)> {
    let (profile, model) = config.selection_for(tilo_desktop::feedback::PACKAGE)?;
    Some((
        profile.id.clone(),
        model.id.clone(),
        format!("{} {}", profile.display_label(), model.display_label()),
    ))
}

/// A connection to this session's model gateway, made for the composer the
/// way the launcher makes one for an app it starts.
fn gateway() -> Option<std::os::fd::OwnedFd> {
    let name = launcher::services::MODEL_GATEWAY_SERVICE;
    let requirement = launcher::manifest::ServiceReq::parse(
        name,
        &format!("optional@{}", runtime_portal::RUNTIME_INTERFACE_VERSION),
    )
    .ok()?;
    let wanted = std::collections::BTreeMap::from([(name.to_owned(), requirement)]);
    let context = launcher::services::ServiceLaunchContext {
        app_id: tilo_desktop::feedback::PACKAGE,
        workspace_id: None,
    };
    match launcher::services::resolve_service_binds_for_launch(&wanted, false, context) {
        Ok(resolved) => resolved.into_iter().next().map(|service| service.host),
        Err(error) => {
            tracing::debug!(%error, "no model gateway for reviewing reports");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::selection;
    use agent_provider_config::{
        AgentModel, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
    };

    /// The review asks the model chosen for the report composer, else the
    /// default, and names the provider and model; with none, no model.
    #[test]
    fn the_review_asks_the_composers_model_by_name() {
        let mut profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        profile.label = "Anthropic".into();
        profile.models = vec![
            ModelDescriptor::new("claude-sonnet-5-5"),
            ModelDescriptor::new("claude-haiku-5-5"),
        ];
        let mut config = ProviderConfig {
            default_profile: Some(profile.id.clone()),
            profiles: vec![profile.clone()],
            ..ProviderConfig::default()
        };
        assert_eq!(selection(&config), None);
        config.profiles[0].default_model = Some("claude-sonnet-5-5".into());
        assert_eq!(
            selection(&config).map(|(_, model, name)| (model, name)),
            Some((
                "claude-sonnet-5-5".into(),
                "Anthropic claude-sonnet-5-5".into()
            ))
        );
        config
            .set_agent_model(
                tilo_desktop::feedback::PACKAGE,
                Some(AgentModel {
                    profile: profile.id,
                    model: "claude-haiku-5-5".into(),
                }),
            )
            .unwrap();
        assert_eq!(
            selection(&config).map(|(_, _, name)| name),
            Some("Anthropic claude-haiku-5-5".into())
        );
    }
}
