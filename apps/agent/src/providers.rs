//! Agent-runner view of the model assignment for planning.
//!
//! The agent never names a provider or a model. It asks the gateway which
//! model the person assigned to `agent.planning` for this agent, captures
//! that [`ModelRoute`] when a run begins, and writes its request in the route's
//! dialect. The gateway re-resolves on every call and refuses a body written
//! for a dialect the assignment no longer uses, so a Settings change cannot
//! silently send work in progress somewhere else. Credentials stay in the
//! gateway; the route carries none.

use agent_provider_config::tasks;
pub use tilo_model_gateway::Route as ModelRoute;

/// The task every agent plan is served as.
pub const PLANNING_TASK: &str = tasks::AGENT_PLANNING.id;

/// The model the person assigned to planning for `agent`, if any serves it.
pub fn planning_route(agent: &str) -> Option<ModelRoute> {
    match tilo_model_gateway::resolve(PLANNING_TASK, Some(agent)) {
        Ok(route) => Some(route),
        Err(error) => {
            tracing::info!(%error, "no model serves agent planning");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A planning route names a model and never an endpoint or secret, and the
    /// packaged agent has no route of its own to a provider or an
    /// environment key: the desktop-owned gateway is the only way out.
    #[test]
    fn a_provider_is_reached_only_through_the_gateway_with_an_explicit_model_and_no_secret() {
        // Case `a_route_carries_a_model_but_no_endpoint_or_secret`.
        {
            let route = ModelRoute {
                task: PLANNING_TASK.into(),
                profile_id: "azure".into(),
                profile_label: "Azure".into(),
                protocol: agent_provider_config::ProviderProtocol::AzureOpenAi,
                location: agent_provider_config::EndpointLocation::Hosted,
                model: agent_provider_config::ModelDescriptor::new("gpt-5.6-sol"),
                options: agent_provider_config::ModelOptions::default(),
                layer: agent_provider_config::assign::Layer::Agent,
            };
            let serialised = serde_json::to_string(&route).unwrap();
            assert!(!serialised.contains("https://"));
            assert!(!serialised.contains("secret"));
        }

        // Case `packaged_agent_has_no_direct_provider_or_environment_key_route`.
        {
            let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src/");
            let model = std::fs::read_to_string(format!("{root}model.rs")).unwrap();
            let providers = std::fs::read_to_string(format!("{root}providers.rs")).unwrap();
            assert!(model.contains("tilo_model_gateway::plan"));
            assert!(!model.contains("ANTHROPIC_API_KEY"));
            assert!(!model.contains("reqwest::"));
            // This scan now reads the file it is written in, so the needle is
            // assembled at run time: spelling it out here would find itself.
            let secret_resolver = ["Secret", "Resolver"].concat();
            assert!(!providers.contains(&secret_resolver));
        }
    }
}
