//! The person's agent model providers, as their settings hold them. Each
//! section changes only through its verbs, which keep the device's
//! providers and the person's hosted ones apart.

use agent_provider_config::{EndpointLocation, HostedProfiles, ProviderConfig, ProviderProfile};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Refusal};

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
impl AgentProviders {
    /// Keep the providers of this device and its network as `config` holds
    /// them, with the default and the models chosen for agents and for each
    /// kind of interaction. A hosted provider is the person's, kept with
    /// their hosted providers, and refused here.
    #[action(id = "set-providers")]
    pub fn set_providers(
        &mut self,
        #[param("The configuration, as the agent provider settings write it")]
        config: serde_json::Value,
    ) -> Result<(), Refusal> {
        let config: ProviderConfig = decoded(config)?;
        only_located(&config.profiles, false)?;
        self.config = config;
        Ok(())
    }

    /// Keep the provider of the models this device runs on what its model
    /// manager serves: the manager's change, nobody's undo step.
    #[action(id = "follow-local-models", external)]
    pub fn follow_local_models(
        &mut self,
        #[param("The local provider, as the agent provider settings write it")]
        profile: serde_json::Value,
    ) -> Result<(), Refusal> {
        let profile: ProviderProfile = decoded(profile)?;
        only_located(std::slice::from_ref(&profile), false)?;
        self.config.upsert(profile);
        Ok(())
    }
}

/// `value` as a `T`, or why it is none.
fn decoded<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, Refusal> {
    serde_json::from_value(value).map_err(|error| Refusal::new(error.to_string()))
}

/// Refuse `profiles` unless each is hosted when `hosted`, or none is
/// otherwise: the hosted ones roam with the person, the others stay on the
/// device.
fn only_located(profiles: &[ProviderProfile], hosted: bool) -> Result<(), Refusal> {
    match profiles
        .iter()
        .find(|profile| (profile.location == EndpointLocation::Hosted) != hosted)
    {
        Some(profile) if hosted => Err(Refusal::new(format!(
            "`{}` is not a hosted provider: it stays on its device",
            profile.id
        ))),
        Some(profile) => Err(Refusal::new(format!(
            "`{}` is a hosted provider: it is the person's, kept with their hosted providers",
            profile.id
        ))),
        None => Ok(()),
    }
}

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
impl HostedAgentProviders {
    /// Keep the person's hosted providers as `hosted` holds them. A provider
    /// of a device or its network stays on that device, and is refused here.
    #[action(id = "set-providers")]
    pub fn set_providers(
        &mut self,
        #[param("The hosted providers, as the agent provider settings write them")]
        hosted: serde_json::Value,
    ) -> Result<(), Refusal> {
        let hosted: HostedProfiles = decoded(hosted)?;
        only_located(&hosted.profiles, true)?;
        self.hosted = hosted;
        Ok(())
    }
}

/// The whole configuration `device` and `person` hold.
#[must_use]
pub fn merged(device: &AgentProviders, person: &HostedAgentProviders) -> ProviderConfig {
    ProviderConfig::from_stored(device.config.clone(), person.hosted.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_provider_config::ProviderProtocol;
    use ui::tilo_resource::Replica;

    /// The device keeps its providers and the person their hosted ones:
    /// each section refuses the other's, and the local provider follows
    /// the model manager without touching the rest.
    #[test]
    fn each_section_keeps_its_own_providers() {
        let hosted = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
        let mut local = hosted.clone();
        local.id = "local".into();
        local.location = EndpointLocation::ThisDevice;
        let config = |profiles: Vec<ProviderProfile>| {
            serde_json::to_value(ProviderConfig {
                profiles,
                ..ProviderConfig::default()
            })
            .unwrap()
        };

        let mut device = Replica::<AgentProviders>::new();
        assert!(
            device
                .apply(AgentProviders::SET_PROVIDERS.with(config(vec![hosted.clone()])))
                .is_err()
        );
        device
            .apply(AgentProviders::SET_PROVIDERS.with(config(vec![local.clone()])))
            .unwrap();
        assert_eq!(device.state().unwrap().config.profiles, [local.clone()]);
        let mut followed = local.clone();
        followed.endpoint = "http://127.0.0.1:43127/v1".into();
        device
            .apply(
                AgentProviders::FOLLOW_LOCAL_MODELS.with(serde_json::to_value(&followed).unwrap()),
            )
            .unwrap();
        assert_eq!(device.state().unwrap().config.profiles, [followed]);

        let mut person = Replica::<HostedAgentProviders>::new();
        let profiles = |profiles: Vec<ProviderProfile>| {
            serde_json::to_value(HostedProfiles { profiles }).unwrap()
        };
        assert!(
            person
                .apply(HostedAgentProviders::SET_PROVIDERS.with(profiles(vec![local])))
                .is_err()
        );
        person
            .apply(HostedAgentProviders::SET_PROVIDERS.with(profiles(vec![hosted.clone()])))
            .unwrap();
        assert_eq!(person.state().unwrap().hosted.profiles, [hosted]);
    }
}
