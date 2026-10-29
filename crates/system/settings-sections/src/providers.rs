//! The person's agent model providers, as their settings hold them.

use agent_provider_config::{HostedProfiles, ProviderConfig};
use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// The agent providers on this device or its network, and which one it uses
/// by default: this device's.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[serde(from = "StoredProviders", into = "StoredProviders")]
pub struct AgentProviders {
    #[model(readonly)]
    pub config: ProviderConfig,
}

/// [`AgentProviders`] as the settings section stores them: without the
/// hosted providers, which are the person's ([`HostedAgentProviders`]).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredProviders {
    config: ProviderConfig,
}

impl From<StoredProviders> for AgentProviders {
    fn from(stored: StoredProviders) -> Self {
        Self {
            config: ProviderConfig::from_stored(stored.config, HostedProfiles::default()),
        }
    }
}

impl From<AgentProviders> for StoredProviders {
    fn from(providers: AgentProviders) -> Self {
        Self {
            config: providers.config.stored().0,
        }
    }
}

impl ui::storage::SettingsSchema for AgentProviders {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(agent_provider_config::SETTINGS_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.agent-providers", label = "Agent model providers")]
impl AgentProviders {}

/// The person's hosted agent providers: each names an endpoint on the
/// internet and a reference to its key, never the key.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[serde(default)]
pub struct HostedAgentProviders {
    #[model(readonly)]
    pub hosted: HostedProfiles,
}

impl ui::storage::SettingsSchema for HostedAgentProviders {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(agent_provider_config::HOSTED_SECTION);
}

#[ui::tilo_resource::resource(
    kind = "settings.hosted-agent-providers",
    label = "Hosted agent model providers"
)]
impl HostedAgentProviders {}

/// The whole configuration `device` and `person` hold.
#[must_use]
pub fn merged(device: &AgentProviders, person: &HostedAgentProviders) -> ProviderConfig {
    ProviderConfig::from_stored(device.config.clone(), person.hosted.clone())
}
