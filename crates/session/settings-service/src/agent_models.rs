//! The person's agent model choice, as a shell's canvas reads and changes it
//! (`settings:settings.agent-models`).
//!
//! The choice is the Agent Models pane's: which providers the person set up,
//! each with the models it offers and the one agents use from it, and which
//! provider and model agents answer with now. The pane reads it from the
//! device's providers section ([`agent_provider_config::SETTINGS_SECTION`])
//! and the person's hosted one ([`agent_provider_config::HOSTED_SECTION`]),
//! through [`ProviderConfig::from_stored`], so this is the same reading of the
//! two together, which a tilolang view reads as `settings.agent_models`.
//!
//! "Use model" ([`AgentModelsEffect::UseModel`]) does what the pane's does:
//! [`ProviderConfig::take_model`] makes the model the one agents use from its
//! provider and the provider the one agents answer with, and both provider
//! sections are written under one change ([`SettingsDocument::update_sections`]),
//! so it is one step the person takes back whole ([`AgentModelsEffect::UndoModel`],
//! over both sections at once). No endpoint, credential or key is read: the
//! reading names providers, models and what agents answer with, nothing more.

use agent_provider_config::{HOSTED_SECTION, ModelDescriptor, ProviderConfig, SETTINGS_SECTION};
use serde::{Deserialize, Serialize};
use settings_store::ChangeOrigin;
use tilo_actions::{ActionAddress, ActionInvoker, ApiError};
use ui::storage::SettingsDocument;
use ui::tilo_resource::Effect;

/// The resource the canvas reads and changes the agent's model through.
pub const RESOURCE: &str = "settings:settings.agent-models";
/// Its kind, as the reading's type is named and a view reads `settings.agent_models`.
pub const KIND: &str = "settings.agent-models";

/// One model a provider offers.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct AgentModelShown {
    /// The model's id, as the provider's API names it.
    pub id: String,
    /// What it is called, as its provider named it.
    pub label: String,
}

/// One provider the person set up, as a composer's switcher shows it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct AgentProviderShown {
    /// The provider's id, as every action names it.
    pub id: String,
    /// What the person calls it.
    pub label: String,
    /// Whether agents may use it.
    pub enabled: bool,
    /// The models it offers.
    pub models: Vec<AgentModelShown>,
    /// The model agents use from it, by id; empty while it has none.
    pub model: String,
    /// What that model is called; empty while it has none.
    pub model_label: String,
}

/// The model agents answer with, as the switcher shows it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct AgentModelInUse {
    /// The provider agents answer with, by id; empty while none does.
    pub provider: String,
    /// Its model, by id; empty while none does.
    pub model: String,
    /// What that model is called; empty while none does.
    pub model_label: String,
}

/// The person's agent model choice: the providers they set up, and the model
/// and provider agents answer with now. No endpoint, credential or key.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct AgentModels {
    /// The providers the person set up, in their own order.
    pub providers: Vec<AgentProviderShown>,
    /// The provider and model agents answer with now.
    pub in_use: AgentModelInUse,
}

impl AgentModels {
    /// The choice `config` holds, as the switcher shows it.
    #[must_use]
    pub fn of(config: &ProviderConfig) -> Self {
        let shown = |model: &ModelDescriptor| AgentModelShown {
            id: model.id.clone(),
            label: model.display_label().to_owned(),
        };
        let label = |profile: &agent_provider_config::ProviderProfile, id: &str| {
            profile
                .models
                .iter()
                .find(|model| model.id == id)
                .map_or_else(String::new, |model| model.display_label().to_owned())
        };
        let providers: Vec<AgentProviderShown> = config
            .profiles
            .iter()
            .map(|profile| {
                let model = profile.default_model.clone().unwrap_or_default();
                AgentProviderShown {
                    id: profile.id.clone(),
                    label: profile.display_label().to_owned(),
                    enabled: profile.enabled,
                    models: profile.models.iter().map(shown).collect(),
                    model_label: label(profile, &model),
                    model,
                }
            })
            .collect();
        let in_use = config
            .active_profile()
            .map_or_else(AgentModelInUse::default, |profile| {
                let model = profile.default_model.clone().unwrap_or_default();
                AgentModelInUse {
                    provider: profile.id.clone(),
                    model_label: label(profile, &model),
                    model,
                }
            });
        Self { providers, in_use }
    }

    /// The configuration as the choice "Use model" leaves it: `provider`'s
    /// model `model`, and `provider` the one agents answer with.
    ///
    /// # Errors
    /// [`ProviderChoiceError`](agent_provider_config::ProviderChoiceError) as
    /// [`ProviderConfig::take_model`] answers it.
    pub fn choose(
        config: &mut ProviderConfig,
        provider: &str,
        model: &str,
    ) -> Result<(), agent_provider_config::ProviderChoiceError> {
        config.take_model(provider, model, None)?;
        // Answering with a model means answering with its provider too: a
        // model on a provider agents do not use changes nothing they plan.
        config.default_profile = Some(provider.to_owned());
        Ok(())
    }
}

/// What the agent-models resource offers: the one choice "Use model" makes,
/// and the taking back and making again of it. The verbs are `undo-model`
/// and `redo-model`, not the framework's own `undo` and `redo` (a step of a
/// resource's history), which this resource is not: the choice spans two
/// documents, so the service takes it back itself.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.settings.agent-models")]
pub enum AgentModelsEffect {
    /// Answer with this model: make its provider the one agents use, and the
    /// model the one they plan with
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    UseModel {
        /// The provider's id
        #[param("The provider's id")]
        provider: String,
        /// The model's id, or any the provider takes
        #[param("The model's id")]
        model: String,
    },
    /// Take back the last choice of a model, on either provider section
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    UndoModel,
    /// Make the last choice of a model again
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    RedoModel,
}

/// The agent model choice as a declared type: the providers the person set
/// up and the model agents answer with.
#[derive(tilo_resource::Row)]
#[row(kind = "settings.agent-models", label = "Agent models")]
pub struct AgentModelsResource;

/// The reading and its actions, as the settings service publishes them.
pub(crate) struct Served {
    /// The settings version the reading was last read at, and the reading.
    seen: std::cell::Cell<Option<u64>>,
    published: std::cell::RefCell<Option<AgentModels>>,
}

impl Served {
    pub(crate) fn new() -> Self {
        Self {
            seen: std::cell::Cell::new(None),
            published: std::cell::RefCell::default(),
        }
    }

    /// The choice the settings now hold, read from both provider sections,
    /// and only when the settings document moved since the last read.
    pub(crate) fn reading(&self) -> AgentModels {
        let stamp = settings_store::stamp().map(|stamp| stamp.0);
        if self.seen.get() == stamp
            && let Some(published) = self.published.borrow().as_ref()
        {
            return published.clone();
        }
        let reading = AgentModels::of(&ProviderConfig::load());
        self.seen.set(stamp);
        *self.published.borrow_mut() = Some(reading.clone());
        reading
    }

    /// The published resource: its reading's fields and its verbs.
    pub(crate) fn resource() -> tilo_actions::ResourceSpec {
        tilo_service::offer::<AgentModelsEffect>(
            tilo_resource::typed_resource::<AgentModelsResource>(
                RESOURCE,
                crate::i18n::t("settings-agent-models"),
            ),
        )
    }

    /// The reading of the resource, as JSON; `None` for another resource.
    pub(crate) fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == RESOURCE)
            .then(|| serde_json::to_value(self.reading()).unwrap_or(serde_json::Value::Null))
    }

    /// Carry out one of the resource's actions as `invoker`, writing the
    /// provider sections the choice touches under one change.
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
            tilo_service::decode::<AgentModelsEffect>(published, resource, action, arguments)?;
        Self::apply(effect, invoker)?;
        // The next reader reads the settings as the change left them.
        self.seen.set(None);
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }

    /// Write the chosen provider and model into both provider sections under
    /// one change, or take the last such change back from both.
    fn apply(effect: AgentModelsEffect, invoker: &ActionInvoker) -> Result<(), ApiError> {
        let document = SettingsDocument::Session;
        let actor = serde_json::to_string(&invoker.acting.by).unwrap_or_default();
        let step = format!("{RESOURCE}/use-model");
        match effect {
            AgentModelsEffect::UseModel { provider, model } => {
                let current = ProviderConfig::load();
                let mut chosen = current.clone();
                AgentModels::choose(&mut chosen, &provider, &model)
                    .map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
                let chosen = chosen.sanitised();
                let was = current.stored();
                let now = chosen.stored();
                let origin = ChangeOrigin::action(step).by(&actor);
                let sections = changed_sections(&was, &now)?;
                if sections.is_empty() {
                    return Ok(());
                }
                document
                    .update_sections(sections, &origin)
                    .map_err(|error| ApiError::Failed(error.to_string()))
            }
            AgentModelsEffect::UndoModel | AgentModelsEffect::RedoModel => {
                let undo = matches!(effect, AgentModelsEffect::UndoModel);
                // A step of this resource's: the choice itself, and the undos
                // and redos of it, as a section's undo finds its own (a
                // step's undo matches the section's actions by their prefix,
                // which is what its redo then brings back).
                let actions = format!("{RESOURCE}/");
                let owns = |version: &tilo_resource::history::Version| {
                    version
                        .message
                        .as_deref()
                        .is_some_and(|message| ui::storage::made_by(message, &actions, &actor))
                };
                let origin = ChangeOrigin::action(format!(
                    "{RESOURCE}/{}",
                    if undo { "undo" } else { "redo" }
                ))
                .by(&actor);
                document
                    .edit_documents(&origin, |doc| {
                        let taken = if undo {
                            ui::history_source::steps::undo_where(
                                doc,
                                &owns,
                                1,
                                |_, _| Ok(None),
                                &ui::history_source::ThreadWords,
                            )
                        } else {
                            ui::history_source::steps::redo_where(
                                doc,
                                &owns,
                                1,
                                |_, _| Ok(None),
                                &ui::history_source::ThreadWords,
                            )
                        };
                        taken.map(drop)
                    })
                    .map_err(|error| ApiError::Failed(error.to_string()))
            }
        }
    }
}

/// One section's change: its name, and the leaves to merge into it.
pub(crate) type SectionChange = (String, serde_json::Map<String, serde_json::Value>);
/// The providers as they are stored: this device's and the person's hosted ones.
type StoredProviders = (ProviderConfig, agent_provider_config::HostedProfiles);

/// The sections of the two provider sections that differ between `before` and
/// `after`, as the leaves each carries, so a write lands only what changed.
pub(crate) fn changed_sections(
    before: &StoredProviders,
    after: &StoredProviders,
) -> Result<Vec<SectionChange>, ApiError> {
    let local = |config: &ProviderConfig| {
        serde_json::to_value(config).map_err(|error| ApiError::Failed(error.to_string()))
    };
    let hosted = |hosted: &agent_provider_config::HostedProfiles| {
        serde_json::to_value(hosted).map_err(|error| ApiError::Failed(error.to_string()))
    };
    let mut sections = Vec::new();
    let (was, now) = (local(&before.0)?, local(&after.0)?);
    if was != now {
        sections.push((SETTINGS_SECTION.to_owned(), one_leaf("config", now)));
    }
    let (was, now) = (hosted(&before.1)?, hosted(&after.1)?);
    if was != now {
        sections.push((HOSTED_SECTION.to_owned(), one_leaf("hosted", now)));
    }
    Ok(sections)
}

/// `value` under its `leaf`, as a section's change carries it.
fn one_leaf(leaf: &str, value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut values = serde_json::Map::new();
    values.insert(leaf.to_owned(), value);
    values
}

/// The resource as a view of another language reads it (`settings.agent_models`):
/// its reading's fields and its verbs, which the shell's runtime hands a
/// tilolang view and sends its `use-model` back through the session's API.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<AgentModels, AgentModelsEffect>(KIND, "Agent model")
}
