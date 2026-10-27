//! The person's agent model providers, as their settings hold them.

use agent_provider_config::ProviderConfig;
use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// The person's agent providers, as their settings hold them: the hosted
/// ones roam with the person, those on this device or its network stay.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[serde(from = "StoredProviders", into = "StoredProviders")]
pub struct AgentProviders {
    #[model(readonly)]
    pub config: ProviderConfig,
}

/// [`AgentProviders`] as the settings section stores them.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredProviders {
    config: ProviderConfig,
    hosted: agent_provider_config::HostedProfiles,
}

impl From<StoredProviders> for AgentProviders {
    fn from(stored: StoredProviders) -> Self {
        Self {
            config: ProviderConfig::from_stored(stored.config, stored.hosted),
        }
    }
}

impl From<AgentProviders> for StoredProviders {
    fn from(providers: AgentProviders) -> Self {
        let (config, hosted) = providers.config.stored();
        Self { config, hosted }
    }
}

impl ui::storage::SettingsSchema for AgentProviders {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(agent_provider_config::SETTINGS_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.agent-providers", label = "Agent model providers")]
impl AgentProviders {}
