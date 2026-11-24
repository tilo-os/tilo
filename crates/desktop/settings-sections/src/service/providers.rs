//! The agent model providers the person set up, and what changes them
//! (`settings:settings.agent-provider-setup`).
//!
//! The providers are the Agent Models pane's: each known provider as a
//! profile (its name, protocol, endpoint and models), the model agents use
//! from it, whether agents may use it, and whether its key is kept. A
//! profile's key lives in the workspace's vault, never in the settings
//! document; the reading says whether the profile names it and never what it
//! is.
//!
//! What changes a provider is the same change the pane makes: the profile
//! is edited and written to the provider section it belongs to, under one
//! change of the person who made it. A provider's key is kept, forgotten and
//! taken away with its provider by `tilo.models` (`set-key`, `remove-key`,
//! `remove-provider`), which keeps it in the workspace's vault; the reading
//! here says whether the profile names one.
//!
//! Signing a provider in (Google, ChatGPT, GitHub Copilot) and downloading a
//! local model are attended flows the device's other services run, and are
//! not offered here yet.

use agent_provider_config::{
    EndpointLocation, ProviderConfig, ProviderProfile, ProviderProtocol, presets,
};
use serde::{Deserialize, Serialize};
use settings_store::ChangeOrigin;
use tilo_actions::{ActionAddress, ActionInvoker, ApiError};
use ui::storage::SettingsDocument;
use ui::tilo_resource::Effect;

use super::agent_models::changed_sections;

/// The resource the pane reads and changes the providers through.
pub const RESOURCE: &str = "settings/agent-provider-setup";
/// Its kind, as the reading's type is named and a view reads
/// `settings.agent_providers`.
pub const KIND: &str = "settings.agent-provider-setup";

/// One model a provider offers.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct AgentProviderModelShown {
    /// The model's id, as the provider's API names it.
    pub id: String,
    /// What it is called.
    pub label: String,
}

/// One provider the person set up.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct AgentProviderShown {
    /// The provider's id, as every action names it.
    pub id: String,
    /// What the person calls it.
    pub label: String,
    /// `anthropic` or `openai-compatible`.
    pub protocol: String,
    /// Where it answers.
    pub endpoint: String,
    /// `hosted`, `this-device` or `private-network`.
    pub location: String,
    /// Whether agents may use it.
    pub enabled: bool,
    /// The models it offers.
    pub models: Vec<AgentProviderModelShown>,
    /// The model agents use from it, by id; empty while it has none.
    pub model: String,
    /// What that model is called; empty while it has none.
    pub model_label: String,
    /// Whether agents answer with it now.
    pub in_use: bool,
    /// How it authenticates: `api-key`, `sign-in` or `none`.
    pub authentication: String,
    /// Where its key stands: `stored`, `missing`, `signed-in` or `not-needed`.
    pub key: String,
    /// Who signed it in, when it is signed in: `google`, `chatgpt`, `github-copilot` or
    /// `huggingface`. Empty otherwise.
    pub sign_in: String,
}

/// One provider the person can add.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct AgentProviderPresetShown {
    /// The name `add-provider` takes.
    pub slug: String,
    /// What the person calls it.
    pub label: String,
    /// What it is, in one line.
    pub description: String,
    /// `anthropic` or `openai-compatible`.
    pub protocol: String,
    /// How a new one authenticates: `api-key`, `sign-in` or `none`.
    pub authentication: String,
}

/// The person's agent model providers and the ones they can add.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct AgentProviderSetup {
    /// The providers the person set up, in their own order.
    pub providers: Vec<AgentProviderShown>,
    /// The provider agents use unless told otherwise; empty while none.
    pub default_provider: String,
    /// The known providers a person can add without signing in.
    pub presets: Vec<AgentProviderPresetShown>,
}

impl AgentProviderSetup {
    /// The providers `config` holds, with `key` saying where each key stands.
    #[must_use]
    pub fn of(config: &ProviderConfig, key: &dyn Fn(&ProviderProfile) -> String) -> Self {
        let shown = |model: &agent_provider_config::ModelDescriptor| AgentProviderModelShown {
            id: model.id.clone(),
            label: model.display_label().to_owned(),
        };
        let label = |profile: &ProviderProfile, id: &str| {
            profile
                .models
                .iter()
                .find(|model| model.id == id)
                .map_or_else(String::new, |model| model.display_label().to_owned())
        };
        let in_use = config
            .active_profile()
            .map(|profile| profile.id.clone())
            .unwrap_or_default();
        let providers: Vec<AgentProviderShown> = config
            .profiles
            .iter()
            .map(|profile| {
                let model = profile.default_model.clone().unwrap_or_default();
                let sign_in = profile
                    .sign_in
                    .as_ref()
                    .and_then(|credential| credential.issuer())
                    .map(|issuer| issuer.id().to_owned())
                    .unwrap_or_default();
                AgentProviderShown {
                    id: profile.id.clone(),
                    label: profile.display_label().to_owned(),
                    protocol: protocol_slug(profile.protocol).to_owned(),
                    endpoint: profile.endpoint.clone(),
                    location: location_slug(profile.location).to_owned(),
                    enabled: profile.enabled,
                    models: profile.models.iter().map(shown).collect(),
                    model_label: label(profile, &model),
                    model,
                    in_use: profile.id == in_use,
                    authentication: authentication_of(profile).to_owned(),
                    key: key(profile),
                    sign_in,
                }
            })
            .collect();
        let presets = presets::Preset::ALL
            .iter()
            .copied()
            .filter(|preset| !preset.signs_in_only())
            .map(|preset| AgentProviderPresetShown {
                slug: preset.slug().to_owned(),
                label: preset.label().to_owned(),
                description: preset.description().to_owned(),
                protocol: protocol_slug(preset.protocol()).to_owned(),
                authentication: preset.authentication().slug().to_owned(),
            })
            .collect();
        Self {
            providers,
            default_provider: in_use,
            presets,
        }
    }
}

/// What the resource offers: adding a provider from the catalogue, and
/// changing its name, endpoint, protocol, model and whether agents may use it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.settings.agent-providers")]
pub enum AgentProvidersEffect {
    /// Add a provider from a known one, which fills its name, endpoint,
    /// protocol and key header; its key and model are set afterwards
    #[effect(kind = "outside")]
    #[effect(own_device)]
    AddProvider {
        /// The preset's name, as the reading lists it
        #[param("A provider's name, as this reading lists it")]
        preset: String,
    },
    /// Offer a provider to agents, or stop offering it
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    SetProviderEnabled {
        /// The provider's id
        #[param("The provider's id")]
        provider: String,
        /// Whether agents may use it
        #[param("Whether agents may use it")]
        on: bool,
    },
    /// Name a provider, where it answers, and the model agents use from it
    #[effect(kind = "outside")]
    #[effect(own_device)]
    SetProviderDetails {
        /// The provider's id
        #[param("The provider's id")]
        provider: String,
        /// Its name
        #[param("Its name")]
        label: String,
        /// Its endpoint URL
        #[param("Its endpoint URL")]
        endpoint: String,
        /// The model agents use from it; omitted leaves the choice as it is
        #[param("The model agents use from it")]
        model: Option<String>,
    },
    /// Speak another protocol to a provider
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    SetProviderProtocol {
        /// The provider's id
        #[param("The provider's id")]
        provider: String,
        /// `anthropic` or `openai-compatible`
        #[param(
            "`anthropic` or `openai-compatible`",
            choices = ["anthropic", "openai-compatible"]
        )]
        protocol: String,
    },
    /// Make one model the one agents use from this provider
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    SetProviderModel {
        /// The provider's id
        #[param("The provider's id")]
        provider: String,
        /// The model's id
        #[param("The model's id")]
        model: String,
    },
    /// Make a provider the one agents use unless told otherwise
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    SetDefaultProvider {
        /// The provider's id
        #[param("The provider's id")]
        provider: String,
    },
    /// Read the providers and their keys again, after another window or a
    /// script changed them
    #[effect(kind = "read")]
    RefreshProviders,
}

/// The providers as a declared type: what the pane reads and changes.
#[derive(tilo_resource::Row)]
#[row(kind = "settings.agent-provider-setup", label = "Agent provider setup")]
pub struct AgentProviderSetupResource;

/// The reading and its actions, as the settings service publishes them.
pub(crate) struct Served {
    /// The settings version the reading was last read at, and the reading.
    seen: std::cell::Cell<Option<u64>>,
    published: std::cell::RefCell<Option<AgentProviderSetup>>,
}

impl Served {
    pub(crate) fn new() -> Self {
        Self {
            seen: std::cell::Cell::new(None),
            published: std::cell::RefCell::default(),
        }
    }

    /// The providers the settings now hold, and where their keys stand,
    /// read only when the settings document moved since the last read.
    pub(crate) fn reading(&self) -> AgentProviderSetup {
        let stamp = settings_store::stamp().map(|stamp| stamp.0);
        if self.seen.get() == stamp
            && let Some(published) = self.published.borrow().as_ref()
        {
            return published.clone();
        }
        let config = ProviderConfig::load();
        let reading = AgentProviderSetup::of(&config, &key_state);
        self.seen.set(stamp);
        *self.published.borrow_mut() = Some(reading.clone());
        reading
    }

    /// The published resource: its reading's fields and its verbs.
    pub(crate) fn resource() -> tilo_actions::ResourceSpec {
        tilo_service::offer::<AgentProvidersEffect>(tilo_resource::typed_resource::<
            AgentProviderSetupResource,
        >(
            RESOURCE,
            super::i18n::t("settings-agent-providers"),
        ))
    }

    /// The reading of the resource, as JSON; `None` for another resource.
    pub(crate) fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == RESOURCE)
            .then(|| serde_json::to_value(self.reading()).unwrap_or(serde_json::Value::Null))
    }

    /// Carry out one of the resource's actions as `invoker`, writing the
    /// provider section the change touches.
    pub(crate) fn invoke(
        &self,
        published: &[tilo_actions::ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        invoker: &ActionInvoker,
    ) -> Result<tilo_service::Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        let effect =
            tilo_service::decode::<AgentProvidersEffect>(published, resource, action, arguments)?;
        Self::apply(effect, action, invoker)?;
        // The next reader reads the providers as the change left them.
        self.seen.set(None);
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }

    /// Write what `effect` changes into the provider sections under one
    /// change.
    fn apply(
        effect: AgentProvidersEffect,
        action: &str,
        invoker: &ActionInvoker,
    ) -> Result<(), ApiError> {
        if matches!(effect, AgentProvidersEffect::RefreshProviders) {
            return Ok(());
        }
        let current = ProviderConfig::load();
        let mut config = current.clone();
        match effect {
            AgentProvidersEffect::AddProvider { preset } => {
                let preset = presets::Preset::from_slug(&preset)
                    .filter(|preset| !preset.signs_in_only())
                    .ok_or_else(|| {
                        ApiError::InvalidArguments(
                            "no provider by that name can be added here".to_owned(),
                        )
                    })?;
                config.upsert(preset.profile());
            }
            AgentProvidersEffect::SetProviderEnabled { provider, on } => {
                find(&mut config, &provider)?.enabled = on;
            }
            AgentProvidersEffect::SetProviderDetails {
                provider,
                label,
                endpoint,
                model,
            } => {
                let label = label.trim();
                if label.is_empty()
                    || label.chars().count() > 120
                    || label.chars().any(char::is_control)
                {
                    return Err(ApiError::InvalidArguments(
                        "a provider needs a name of at most 120 letters".to_owned(),
                    ));
                }
                let known_scheme =
                    endpoint.starts_with("https://") || endpoint.starts_with("http://");
                if endpoint.len() > 2048
                    || endpoint.chars().any(char::is_whitespace)
                    || !known_scheme
                {
                    return Err(ApiError::InvalidArguments(
                        "a provider needs an http or https endpoint".to_owned(),
                    ));
                }
                let profile = find(&mut config, &provider)?;
                profile.label = label.to_owned();
                profile.endpoint = endpoint;
                if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
                    profile.default_model = Some(model);
                }
            }
            AgentProvidersEffect::SetProviderProtocol { provider, protocol } => {
                let protocol = protocol::parse(&protocol)?;
                find(&mut config, &provider)?.protocol = protocol;
            }
            AgentProvidersEffect::SetProviderModel { provider, model } => {
                if model.trim().is_empty() {
                    return Err(ApiError::InvalidArguments(
                        "a provider's model needs a name".to_owned(),
                    ));
                }
                find(&mut config, &provider)?.default_model = Some(model);
            }
            AgentProvidersEffect::SetDefaultProvider { provider } => {
                let available = config
                    .profiles
                    .iter()
                    .any(|profile| profile.id == provider && profile.enabled);
                if !available {
                    return Err(ApiError::InvalidArguments(
                        "agents can only answer with a provider they may use".to_owned(),
                    ));
                }
                config.default_profile = Some(provider);
            }
            AgentProvidersEffect::RefreshProviders => unreachable!("answered above"),
        }
        let chosen = config.sanitised();
        let was = current.stored();
        let now = chosen.stored();
        let sections = changed_sections(&was, &now)?;
        if sections.is_empty() {
            return Ok(());
        }
        let actor = serde_json::to_string(&invoker.acting.by).unwrap_or_default();
        let origin = ChangeOrigin::action(format!("{RESOURCE}/{action}")).by(&actor);
        SettingsDocument::Session
            .update_sections(sections, &origin)
            .map_err(|error| ApiError::Failed(error.to_string()))
    }
}

/// The provider `provider` of `config`.
fn find<'a>(
    config: &'a mut ProviderConfig,
    provider: &str,
) -> Result<&'a mut ProviderProfile, ApiError> {
    config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == provider)
        .ok_or_else(|| gone(provider))
}

/// Why a provider named `provider` cannot be changed: it is not one this device keeps.
fn gone(provider: &str) -> ApiError {
    ApiError::InvalidArguments(format!("there is no provider `{provider}`"))
}

/// Where `profile`'s key stands, as the reading says it: a key profile names
/// the vault entry `tilo.models` keeps its key in.
fn key_state(profile: &ProviderProfile) -> String {
    if profile.sign_in.is_some() {
        "signed-in"
    } else if agent_provider_config::is_unauthenticated_local(profile) {
        "not-needed"
    } else if profile.credential.is_some() {
        "stored"
    } else {
        "missing"
    }
    .to_owned()
}

/// How `profile` authenticates, as the reading says it.
fn authentication_of(profile: &ProviderProfile) -> &'static str {
    if profile.sign_in.is_some() {
        "sign-in"
    } else if profile.credential.is_none() {
        "none"
    } else {
        "api-key"
    }
}

/// The name the reading and an action use for `protocol`.
fn protocol_slug(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::OpenAiCompatible => "openai-compatible",
        ProviderProtocol::Anthropic => "anthropic",
    }
}

/// The protocol `slug` names.
mod protocol {
    use super::{ApiError, ProviderProtocol};

    /// Parse the protocol a person chose.
    pub(super) fn parse(slug: &str) -> Result<ProviderProtocol, ApiError> {
        match slug {
            "anthropic" => Ok(ProviderProtocol::Anthropic),
            "openai-compatible" => Ok(ProviderProtocol::OpenAiCompatible),
            other => Err(ApiError::InvalidArguments(format!(
                "`{other}` is not a protocol a provider speaks"
            ))),
        }
    }
}

/// The name the reading uses for `location`.
fn location_slug(location: EndpointLocation) -> &'static str {
    match location {
        EndpointLocation::Hosted => "hosted",
        EndpointLocation::ThisDevice => "this-device",
        EndpointLocation::PrivateNetwork => "private-network",
    }
}

/// The resource as a view of another language reads it (`settings.agent_providers`):
/// its reading's fields and its verbs.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<AgentProviderSetup, AgentProvidersEffect>(
        KIND,
        "Agent provider setup",
    )
}
