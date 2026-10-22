//! Portable provider administration shared by Settings and standalone hosts.
use crate::i18n::{t, t_args};
use crate::widgets::{
    Item, SettingsSection, StatusTone, field_row, note, row, rows, section, semantic_status_pill,
    value_row,
};
use agent_provider_config::management::{CredentialStatus, ProviderAdmin, ProviderSnapshot};
use agent_provider_config::presets::{Authentication, Preset};
use agent_provider_config::protocol::{ErrorKind, ModelError};
use agent_provider_config::{
    AgentModel, EndpointLocation, ModelDescriptor, ModelRequestLimits, ProviderConfig,
    ProviderProfile, ProviderProtocol,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use ui::framework::{ActionControl, LiveRow};
use ui::lock::MutexRecover as _;
use ui::tilo_resource::{self, ActionRef, Binds, Effect};
use ui::widget::{Button, Dropdown, Row, Text, TextInput};
use zeroize::Zeroize;
mod admin;
use admin::AdminRead;
mod app;
#[cfg(test)]
mod app_tests;
pub mod i18n;
pub mod widgets;
pub use app::{ProviderSettingsApp, definition};
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

pub trait PanelBindings: Clone + 'static {
    fn offline_metadata(&self) -> bool {
        false
    }
    fn bind<I: 'static, C: ActionControl<I>>(
        &self,
        control: C,
        binding: impl Binds<ProviderEffect, I>,
    ) -> C;
    fn bind_invocation<C: ActionControl<()>>(&self, control: C, effect: ProviderEffect) -> C;
}
const fn identity<A: serde::Serialize>(
    action: ActionRef<ProviderEffect, A>,
) -> ActionRef<ProviderEffect, A> {
    action
}
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum ProviderEffect {
    /// Reload the authoritative profiles and credential status.
    #[effect(job, label = "Refresh providers")]
    RefreshProviders,
    /// Add a provider from a known one, which fills its name, endpoint and
    /// protocol; its model is chosen afterwards.
    #[effect(job, label = "Add provider")]
    AddProvider {
        #[param(
            "`openai`, `anthropic`, `gemini`, `mistral`, `openrouter`, `groq`, `xai`, `deepseek`, `together`, `fireworks`, `huggingface`, `ollama`, `lm-studio`, or `openai-compatible` for any other"
        )]
        preset: String,
    },
    /// Ask the provider which models it offers, with its stored key or
    /// sign-in: shows whether the connection works, and the models to
    /// choose from.
    #[effect(job, row, label = "Test connection")]
    ListProviderModels {
        #[param("The provider's id")]
        provider: String,
    },
    /// Make one model the one agents use from this provider: one it listed,
    /// or any other id.
    #[effect(job, row, idempotent, label = "Use model")]
    SetProviderModel {
        #[param("The provider's id")]
        provider: String,
        #[param("The model's id")]
        model: String,
    },
    /// Speak another protocol to a provider.
    #[effect(job, row, idempotent)]
    SetProviderProtocol {
        #[param("The provider's id")]
        provider: String,
        #[param("`anthropic` or `openai-compatible`")]
        protocol: String,
    },
    /// Offer a provider to agents, or stop offering it.
    #[effect(job, row, idempotent)]
    SetProviderEnabled {
        #[param("The provider's id")]
        provider: String,
        on: bool,
    },
    /// Name a provider, where it is, and the model agents use from it.
    #[effect(job, row)]
    SetProviderDetails {
        #[param("The provider's id")]
        provider: String,
        #[param("Its name")]
        label: String,
        #[param("Its endpoint URL")]
        endpoint: String,
        #[param("The model agents use")]
        model: Option<String>,
    },
    /// Set this model's request allowance; omit both values to restore defaults.
    #[effect(job, row, idempotent)]
    SetModelRequestLimits {
        provider: String,
        model: String,
        max_output_tokens: Option<u32>,
        runtime_context_tokens: Option<u32>,
        plan_timeout_ms: Option<u32>,
    },
    /// Read source context and this model's configured deployment limits.
    #[effect(row, returns, kind = "read")]
    InspectModelRequestLimits { provider: String, model: String },
    /// Make a provider the one agents use unless told otherwise.
    #[effect(job, row, idempotent, label = "Use by default")]
    SetDefaultProvider {
        #[param("The provider's id")]
        provider: String,
    },
    /// Choose the model one agent plans with; omit both to let it use the
    /// default again.
    #[effect(job, idempotent)]
    SetAgentModel {
        #[param("The agent's id")]
        agent: String,
        #[param("The provider's id")]
        provider: Option<String>,
        #[param("The model's id, one the provider offers")]
        model: Option<String>,
    },
    /// Remove a provider and its stored API key.
    #[effect(job, row, destructive, label = "Remove provider")]
    RemoveProvider {
        #[param("The provider's id")]
        provider: String,
    },
    /// Store a provider's API key in this device's vault: a hosted
    /// provider that roamed here from another device needs its own.
    #[effect(job, row, label = "Save API key")]
    SaveProviderKey {
        #[param("The provider's id")]
        provider: String,
        #[param(secret, "The API key; what the key field holds when omitted")]
        key: Option<String>,
    },
    /// Bind a legacy key only after independently confirming its original endpoint.
    #[effect(job, row, label = "Confirm original endpoint and migrate key")]
    MigrateProviderKey {
        #[param("The provider's id")]
        provider: String,
        #[param("Original endpoint used with this key, not a newly edited endpoint")]
        original_endpoint: String,
    },
    /// Remove a legacy key that is not bound to a provider origin.
    #[effect(job, row, destructive, label = "Remove unbound legacy key")]
    RemoveLegacyProviderKey {
        #[param("The provider's id")]
        provider: String,
    },
    /// Remove a provider's stored API key.
    #[effect(job, row, destructive, label = "Remove API key")]
    RemoveProviderKey {
        #[param("The provider's id")]
        provider: String,
    },
}
fn protocol_slug(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::Anthropic => "anthropic",
        ProviderProtocol::OpenAiCompatible => "openai-compatible",
    }
}

fn protocol(slug: &str) -> Result<ProviderProtocol, String> {
    match slug {
        "anthropic" => Ok(ProviderProtocol::Anthropic),
        "openai-compatible" => Ok(ProviderProtocol::OpenAiCompatible),
        _ => Err("unsupported provider protocol".to_owned()),
    }
}

fn request_limit_rows(profile: &ProviderProfile) -> Vec<LiveRow> {
    profile
        .models
        .iter()
        .map(|selected| request_limit_row(profile, selected))
        .collect()
}

fn request_limit_row(profile: &ProviderProfile, selected: &ModelDescriptor) -> LiveRow {
    LiveRow::new(
        format!(
            "settings.agent-provider-model:{}:{}",
            hex::encode(&profile.id),
            hex::encode(&selected.id)
        ),
        "settings.agent-provider-model",
        selected.display_label().to_owned(),
    )
    .effect(&ProviderEffect::InspectModelRequestLimits {
        provider: profile.id.clone(),
        model: selected.id.clone(),
    })
    .operation(
        ProviderEffect::SetModelRequestLimits {
            provider: profile.id.clone(),
            model: selected.id.clone(),
            max_output_tokens: None,
            runtime_context_tokens: None,
            plan_timeout_ms: None,
        }
        .bound_asking(&[
            "max_output_tokens",
            "runtime_context_tokens",
            "plan_timeout_ms",
        ]),
    )
}

fn provider_row(profile: &ProviderProfile) -> LiveRow {
    let offer = |effect: ProviderEffect| effect.bound();
    let id = profile.id.clone();
    let row = LiveRow::new(
        format!("settings.agent-provider:{id}"),
        "settings.agent-provider",
        profile.display_label().to_owned(),
    )
    .operation(offer(ProviderEffect::SetProviderEnabled {
        provider: id.clone(),
        on: !profile.enabled,
    }))
    .operation(offer(ProviderEffect::SetDefaultProvider {
        provider: id.clone(),
    }))
    .operation(offer(ProviderEffect::RemoveProvider {
        provider: id.clone(),
    }))
    .operation(offer(ProviderEffect::ListProviderModels {
        provider: id.clone(),
    }))
    .operation(
        ProviderEffect::SetProviderModel {
            provider: id.clone(),
            model: profile.default_model.clone().unwrap_or_default(),
        }
        .bound_asking(&["model"]),
    );
    let row = row
        .operation(
            ProviderEffect::SetProviderDetails {
                provider: id.clone(),
                label: profile.label.clone(),
                endpoint: profile.endpoint.clone(),
                model: profile.default_model.clone(),
            }
            .bound_asking(&["label", "endpoint", "model"]),
        )
        .operation(
            ProviderEffect::SetProviderProtocol {
                provider: id.clone(),
                protocol: protocol_slug(profile.protocol).into(),
            }
            .bound_asking(&["protocol"]),
        );
    if profile.location == EndpointLocation::Hosted && profile.sign_in.is_none() {
        row.operation(
            ProviderEffect::SaveProviderKey {
                provider: id,
                key: None,
            }
            .bound_asking(&["key"]),
        )
        .operation(
            ProviderEffect::MigrateProviderKey {
                provider: profile.id.clone(),
                original_endpoint: String::new(),
            }
            .bound_asking(&["original_endpoint"]),
        )
        .operation(offer(ProviderEffect::RemoveLegacyProviderKey {
            provider: profile.id.clone(),
        }))
        .operation(offer(ProviderEffect::RemoveProviderKey {
            provider: profile.id.clone(),
        }))
    } else {
        row
    }
}

struct PanelWake(ui::WakeHandle);
impl std::task::Wake for PanelWake {
    fn wake(self: Arc<Self>) {
        self.0.wake();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.wake();
    }
}
pub struct ProviderPanel<H: PanelBindings> {
    pub cx: H,
    pub provider_admin: ui::models::ProviderAdministration,
    pub offline_config: ProviderConfig,
    pub notice: Option<String>,
    pub changed: bool,
    pub provider_key_buffers: Mutex<HashMap<String, Arc<Mutex<String>>>>,
    provider_field_buffers: Mutex<HashMap<String, HostedFieldBuffers>>,
    limit_field_buffers: Mutex<HashMap<(String, String), LimitFieldBuffers>>,
    pub keys: HashMap<String, Result<CredentialStatus, String>>,
    /// What each provider answered when its models were last asked for.
    pub listed_models: HashMap<String, Result<Vec<ModelDescriptor>, String>>,
    pub admin_snapshot: Option<ProviderSnapshot>,
    empty_config: ProviderConfig,
    pub admin_pending: Option<admin::PendingAdmin>,
    waker: std::task::Waker,
    admin_reply: Option<ui::framework::EffectReply>,
    saved_key: Option<(Arc<Mutex<String>>, zeroize::Zeroizing<String>)>,
    limits_ack: Option<admin::LimitAck>,
}
impl<H: PanelBindings> ProviderPanel<H> {
    pub fn new(
        cx: H,
        provider_admin: ui::models::ProviderAdministration,
        offline_config: ProviderConfig,
    ) -> Self {
        let mut panel = Self {
            cx,
            provider_admin,
            offline_config,
            notice: None,
            changed: true,
            provider_key_buffers: Mutex::default(),
            provider_field_buffers: Mutex::default(),
            limit_field_buffers: Mutex::default(),
            keys: HashMap::new(),
            listed_models: HashMap::new(),
            admin_snapshot: None,
            empty_config: ProviderConfig::default(),
            admin_pending: None,
            waker: std::task::Waker::from(Arc::new(PanelWake(ui::WakeHandle::new()))),
            admin_reply: None,
            saved_key: None,
            limits_ack: None,
        };
        panel.read_keys();
        panel
    }
    /// Nobody here can change providers and there is nothing to keep
    /// locally: the panel has no controls to offer, only who decides.
    pub fn administered_elsewhere(&self) -> bool {
        !self.provider_admin.is_available() && !self.cx.offline_metadata() && self.notice.is_none()
    }
    /// Whether a change can be asked for now.
    fn admits(&self) -> bool {
        !(self.admin_pending.is_some()
            || (!self.provider_admin.is_available() && !self.cx.offline_metadata())
            || (self.provider_admin.is_available() && self.admin_snapshot.is_none()))
    }
    fn admit(&self, button: Button) -> Button {
        if self.admits() {
            button
        } else {
            button.with_enabled(false)
        }
    }
    fn bind_row(
        &self,
        button: Button,
        row: LiveRow,
        binding: impl Binds<ProviderEffect, ()>,
    ) -> Button {
        Self::bind_row_control(self.admit(button), row, binding)
    }
    /// Bind `control` to one of `row`'s published operations.
    fn bind_row_control<I: 'static, C: ActionControl<I>>(
        control: C,
        row: LiveRow,
        binding: impl Binds<ProviderEffect, I>,
    ) -> C {
        let button = control;
        let row = row.into_resource();
        let Some(spec) = row
            .actions
            .iter()
            .find(|spec| spec.id == binding.action_id())
        else {
            return button.unavailable();
        };
        // Entity arguments belong to the published row operation, not its caller.
        // Keep draft reads deferred until activation and out of binding metadata.
        // Normal action-bound controls follow live permissions; the host resolves
        // this row's current operation again before dispatch or confirmation.
        let asked: Vec<_> = spec
            .params()
            .iter()
            .map(|param| param.name.to_string())
            .collect();
        match ui::tilo_actions::ActionBinding::fallible(&row, spec, move |input| {
            let mut args = binding.arguments(input)?;
            args.retain(|name, _| asked.contains(name));
            Ok(args)
        }) {
            Ok(binding) => button.bind_action(binding),
            Err(_) => button.unavailable(),
        }
    }
    fn bind_invocation(&self, button: Button, effect: ProviderEffect) -> Button {
        // A failed initial load can always be retried.
        let button =
            if matches!(effect, ProviderEffect::RefreshProviders) && self.admin_pending.is_none() {
                button
            } else {
                self.admit(button)
            };
        let arguments = effect.arguments();
        if let Some(provider) = arguments
            .get("provider")
            .and_then(serde_json::Value::as_str)
        {
            let row_id = match arguments.get("model").and_then(serde_json::Value::as_str) {
                Some(model)
                    if matches!(
                        effect,
                        ProviderEffect::SetModelRequestLimits { .. }
                            | ProviderEffect::InspectModelRequestLimits { .. }
                    ) =>
                {
                    format!(
                        "settings.agent-provider-model:{}:{}",
                        hex::encode(provider),
                        hex::encode(model)
                    )
                }
                _ => format!("settings.agent-provider:{provider}"),
            };
            let Some(row) = self.rows().into_iter().find(|row| row.id() == row_id) else {
                return button.unavailable();
            };
            let row = row.into_resource();
            let Some(spec) = row.actions.iter().find(|spec| spec.id == effect.id()) else {
                return button.unavailable();
            };
            let mut arguments = arguments;
            arguments.retain(|name, _| spec.params().iter().any(|param| param.name == *name));
            return match ui::tilo_actions::ActionBinding::static_json(&row, spec, arguments) {
                Ok(binding) => button.bind_action(binding),
                Err(_) => button.unavailable(),
            };
        }
        self.cx.bind_invocation(button, effect)
    }
    pub fn sections(&self) -> Vec<SettingsSection> {
        self.sections_with_sign_in(true)
    }

    /// Native hosts own sign-in setup, while request limits remain shared for every model.
    pub fn sections_without_sign_in(&self) -> Vec<SettingsSection> {
        self.sections_with_sign_in(false)
    }

    fn sections_with_sign_in(&self, include_sign_in: bool) -> Vec<SettingsSection> {
        let mut sections = Vec::new();
        if let Some(notice) = &self.notice {
            sections.push(section(&t("agent-service-title"), rows(vec![note(notice)])));
        }
        sections.push(self.hosted_profiles_with_sign_in(include_sign_in));
        if let Some(agents) = self.agent_models_card() {
            sections.push(agents);
        }
        // Limits are per model: with no model yet the section would be a
        // paragraph about nothing.
        if self
            .provider_config()
            .profiles
            .iter()
            .any(|profile| !profile.models.is_empty())
        {
            sections.push(self.request_limits_card());
        }
        sections
    }
    /// Inspect the configured deployment limits for a provider's model.
    ///
    /// # Errors
    /// Returns an error when the provider or model is unavailable.
    pub fn inspect_limits(&self, provider: &str, model: &str) -> Result<serde_json::Value, String> {
        let config = self.provider_config().clone();
        let selected = config
            .profiles
            .iter()
            .find(|item| item.id == provider)
            .and_then(|profile| profile.models.iter().find(|item| item.id == model))
            .ok_or_else(|| t("agent-model-gone"))?;
        Ok(serde_json::json!({
            "provider":provider, "model":model,
            "source_context_tokens":selected.context_window,
            "request_limits":selected.request_limits,
            "plan_timeout_ceiling_ms":selected.request_limits.as_ref()
                .and_then(|limits| limits.plan_timeout_ms).unwrap_or(60_000),
            "caller_may_request_shorter_deadline":true,
            "runtime_context_attested":false, "prompt_fit_proven":false
        }))
    }
    pub fn rows(&self) -> Vec<LiveRow> {
        self.provider_config()
            .profiles
            .iter()
            .flat_map(|profile| {
                let mut rows = vec![provider_row(profile)];
                rows.extend(request_limit_rows(profile));
                rows
            })
            .collect()
    }
    /// Read only through the mount's explicitly granted administration capability.
    pub fn read_keys(&mut self) {
        if self.admin_pending.is_some() || !self.provider_admin.is_available() {
            return;
        }
        if let Ok(admin) = self.provider_admin.get() {
            self.admin_pending = Some(Box::pin(async move {
                AdminRead::load(admin.as_ref(), Ok(())).await
            }));
        }
        self.changed = true;
    }

    pub fn provider_config(&self) -> &ProviderConfig {
        if self.provider_admin.is_available() {
            self.admin_snapshot
                .as_ref()
                .map_or(&self.empty_config, |snapshot| &snapshot.config)
        } else {
            &self.offline_config
        }
    }

    pub fn poll_admin(&mut self) {
        let Some(future) = self.admin_pending.as_mut() else {
            return;
        };
        let mut cx = std::task::Context::from_waker(&self.waker);
        let std::task::Poll::Ready(read) = future.as_mut().poll(&mut cx) else {
            return;
        };
        self.admin_pending = None;
        if let Some(snapshot) = read.snapshot {
            self.admin_snapshot = Some(snapshot);
        } else {
            // A failed reload must not leave an apparently writable stale revision.
            self.admin_snapshot = None;
        }
        self.keys = read.keys;
        let mut value = None;
        let mut listing_failed = None;
        if let Some((provider, listed)) = read.listed {
            value = listed
                .as_ref()
                .ok()
                .and_then(|models| serde_json::to_value(models).ok());
            let listed = listed.map_err(|error| listing_error(&error));
            listing_failed = listed.as_ref().err().cloned();
            self.listed_models.insert(provider, listed);
        }
        let outcome = read.outcome.map_err(|error| {
            if let Some(message) = listing_failed {
                return message;
            }
            if error.kind == ErrorKind::Conflict && self.admin_snapshot.is_some() {
                t("agent-provider-conflict")
            } else if error.kind == ErrorKind::Conflict {
                format!("{} {}", error, t("agent-provider-refresh-required"))
            } else {
                error.to_string()
            }
        });
        if read.acknowledged
            && let Some((buffer, submitted)) = self.saved_key.take()
        {
            let mut draft = buffer.lock_recover();
            if *draft == *submitted {
                draft.zeroize();
            }
        }
        self.saved_key = None;
        if let Some(acknowledgement) = self.limits_ack.take().filter(|_| read.acknowledged) {
            self.acknowledge_limits(&acknowledgement);
        }
        self.notice = Some(match (&outcome, &value) {
            (Ok(()), Some(serde_json::Value::Array(models))) => {
                t_args("agent-test-connected", &[("count", models.len().into())])
            }
            (Ok(()), _) => t(if self.admin_reply.is_some() {
                "agent-provider-saved"
            } else {
                "agent-provider-ready"
            }),
            (Err(error), _) => error.clone(),
        });
        if let Some(reply) = self.admin_reply.take() {
            reply.finish(outcome.map(|()| ui::tilo_actions::ActionOutput {
                value,
                resources: Vec::new(),
            }));
        }
        self.changed = true;
    }

    /// Whether `profile` is hosted and this device's vault has no key for
    /// it: one that roamed here from another device needs its own.
    pub fn needs_key(&self, profile: &ProviderProfile) -> bool {
        profile.location == EndpointLocation::Hosted
            && profile.sign_in.is_none()
            && matches!(
                self.keys.get(&profile.id),
                Some(Ok(
                    CredentialStatus::Missing | CredentialStatus::MigrationRequired
                ))
            )
    }

    /// Apply a provider change to the person's settings.
    ///
    /// # Errors
    /// Returns an error for unsupported effects, missing profiles or invalid details.
    pub fn proposed_config(&self, effect: ProviderEffect) -> Result<ProviderConfig, String> {
        let mut config = self.provider_config().clone();
        match effect {
            ProviderEffect::SetModelRequestLimits {
                provider,
                model,
                max_output_tokens,
                runtime_context_tokens,
                plan_timeout_ms,
            } => {
                set_model_limits(
                    &mut config,
                    &provider,
                    &model,
                    (max_output_tokens, runtime_context_tokens, plan_timeout_ms),
                )?;
            }
            ProviderEffect::AddProvider { preset } => {
                let preset = Preset::from_slug(&preset)
                    .filter(|preset| !matches!(preset.authentication(), Authentication::SignIn(_)))
                    .ok_or_else(|| t("agent-preset-unknown"))?;
                config.upsert(preset.profile());
            }
            ProviderEffect::SetProviderModel { provider, model } => {
                let model = model.trim().to_owned();
                if model.is_empty() || model.len() > 256 || model.contains(char::is_control) {
                    return Err(t("agent-model-invalid"));
                }
                let listed = self
                    .listed_models
                    .get(&provider)
                    .and_then(|listed| listed.as_ref().ok())
                    .and_then(|models| models.iter().find(|listed| listed.id == model))
                    .cloned();
                let profile = find(&mut config, &provider)?;
                if !profile.models.iter().any(|known| known.id == model) {
                    profile
                        .models
                        .push(listed.unwrap_or_else(|| ModelDescriptor::new(model.clone())));
                }
                profile.default_model = Some(model);
            }
            ProviderEffect::SetProviderProtocol {
                provider,
                protocol: slug,
            } => {
                find(&mut config, &provider)?.protocol = protocol(&slug)?;
            }
            ProviderEffect::SetProviderEnabled { provider, on } => {
                find(&mut config, &provider)?.enabled = on;
            }
            ProviderEffect::SetDefaultProvider { provider } => {
                let available = config.profiles.iter().any(|profile| {
                    profile.id == provider && profile.enabled && profile.default_model.is_some()
                });
                if !available {
                    return Err(t("agent-provider-not-default"));
                }
                config.default_profile = Some(provider);
            }
            ProviderEffect::SetProviderDetails {
                provider,
                label,
                endpoint,
                model,
            } => {
                if label.trim().is_empty() || endpoint.trim().is_empty() {
                    return Err(t("agent-provider-details-required"));
                }
                let profile = find(&mut config, &provider)?;
                profile.label = label;
                profile.endpoint = endpoint;
                if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
                    if !profile.models.iter().any(|candidate| candidate.id == model) {
                        profile.models.push(ModelDescriptor::new(model.clone()));
                    }
                    profile.default_model = Some(model);
                }
            }
            ProviderEffect::RemoveProvider { provider } => {
                if config.remove(&provider).is_none() {
                    return Err(t("agent-provider-gone"));
                }
            }
            ProviderEffect::SetAgentModel {
                agent,
                provider,
                model,
            } => {
                let chosen = match (provider, model) {
                    (Some(profile), Some(model)) => Some(AgentModel { profile, model }),
                    (None, None) => None,
                    _ => return Err(t("agent-model-choice-incomplete")),
                };
                config
                    .set_agent_model(&agent, chosen)
                    .map_err(|_| t("agent-model-gone"))?;
            }
            _ => return Err("not a provider effect".into()),
        }
        Ok(config.sanitised())
    }

    pub fn provider_notice(&mut self, outcome: Result<String, &String>) {
        self.notice = Some(match outcome {
            Ok(message) => message,
            Err(error) => error.clone(),
        });
        self.changed = true;
    }

    pub fn key_buffer(&self, profile_id: &str) -> Arc<Mutex<String>> {
        let mut buffers = self.provider_key_buffers.lock_recover();
        buffers
            .entry(profile_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(String::new())))
            .clone()
    }

    pub fn field_buffers(&self, profile: &ProviderProfile) -> HostedFieldBuffers {
        let mut buffers = self.provider_field_buffers.lock_recover();
        buffers
            .entry(profile.id.clone())
            .or_insert_with(|| HostedFieldBuffers {
                original_endpoint: Arc::new(Mutex::new(String::new())),
                label: Arc::new(Mutex::new(profile.label.clone())),
                endpoint: Arc::new(Mutex::new(profile.endpoint.clone())),
                model: Arc::new(Mutex::new(
                    profile.default_model.clone().unwrap_or_default(),
                )),
            })
            .clone()
    }

    pub fn hosted_profiles(&self) -> SettingsSection {
        self.hosted_profiles_with_sign_in(true)
    }

    /// Whether the backend's providers are loaded and nothing is pending.
    pub fn can_admin(&self) -> bool {
        self.provider_admin.is_available()
            && self.admin_pending.is_none()
            && self.admin_snapshot.is_some()
    }

    fn hosted_profiles_with_sign_in(&self, include_sign_in: bool) -> SettingsSection {
        let config = self.provider_config().clone();
        let can_admin = self.can_admin();
        let mut body: Vec<Item> = vec![note(&t("agent-hosted-intro"))];
        if !self.provider_admin.is_available() {
            body.push(note(&t(if self.cx.offline_metadata() {
                "agent-provider-offline"
            } else {
                "agent-provider-no-authority"
            })));
        } else if self.admin_pending.is_some() {
            body.push(note(&t("agent-provider-pending")));
        }
        // Controls nobody here can use are not offered: without provider
        // administration the note above says who can change providers.
        if self.provider_admin.is_available() {
            body.push(row(Box::new(
                self.bind_invocation(
                    Button::new(
                        "agent-models-refresh-providers",
                        &t("agent-provider-refresh"),
                    )
                    .with_enabled(self.admin_pending.is_none()),
                    ProviderEffect::RefreshProviders,
                ),
            )));
        }
        if self.provider_admin.is_available() || self.cx.offline_metadata() {
            body.push(field_row(
                &t("agent-add-provider"),
                Box::new(self.add_provider_control()),
            ));
        }
        for profile in config
            .profiles
            .iter()
            // The local model this device's model service runs has its own card.
            .filter(|profile| {
                profile.location != EndpointLocation::ThisDevice
                    || !profile.is_local_vllm_candidate()
            })
            .filter(|profile| include_sign_in || profile.sign_in.is_none())
        {
            self.profile_body(&mut body, profile, can_admin);
        }
        for issue in config.validation_errors() {
            body.push(note(&t_args(
                "agent-config-issue",
                &[("message", issue.message.as_str().into())],
            )));
        }
        section(&t("agent-hosted-title"), rows(body))
    }

    /// The known providers to add, each filling its own details. Those that
    /// sign in are offered where the sign-in runs (the native host).
    fn add_provider_control(&self) -> Dropdown {
        let mut chooser = Dropdown::from_state(Vec::new(), &ui::LocalState::new(String::new()))
            .with_id("agent-models-add-provider")
            .with_accessible_name(t("agent-add-provider"))
            .with_placeholder(&t("agent-add-provider-placeholder"));
        chooser.set_value_options(
            Preset::ALL
                .into_iter()
                .filter(|preset| !matches!(preset.authentication(), Authentication::SignIn(_)))
                .map(|preset| (preset.slug().to_owned(), preset.label().to_owned()))
                .collect(),
        );
        let chooser = chooser.with_enabled(self.admits());
        self.cx
            .bind(chooser, identity(ProviderEffect::ADD_PROVIDER))
    }

    /// Test the connection, and choose the model from what the provider
    /// listed; the model field above takes any other id.
    pub fn profile_models(&self, body: &mut Vec<Item>, profile: &ProviderProfile, can_admin: bool) {
        let profile_id = profile.id.clone();
        body.push(row(Box::new(
            self.bind_invocation(
                Button::new(
                    &format!("agent-models-test-{profile_id}"),
                    &t("agent-test-connection"),
                )
                .with_enabled(can_admin),
                ProviderEffect::ListProviderModels {
                    provider: profile_id.clone(),
                },
            ),
        )));
        match self.listed_models.get(&profile_id) {
            None => {}
            Some(Err(error)) => body.push(row(widgets::error_caption(error))),
            Some(Ok(models)) if models.is_empty() => {
                body.push(note(&t("agent-test-no-models")));
            }
            Some(Ok(models)) => {
                let selected = profile.default_model.clone().unwrap_or_default();
                let mut chooser = Dropdown::from_state(Vec::new(), &ui::LocalState::new(selected))
                    .with_id(&format!("agent-models-choose-{profile_id}"))
                    .with_accessible_name(t("agent-choose-model"))
                    .with_placeholder(&t("agent-choose-model-placeholder"));
                chooser.set_value_options(
                    models
                        .iter()
                        .map(|model| (model.id.clone(), model.display_label().to_owned()))
                        .collect(),
                );
                let provider = profile_id.clone();
                body.push(field_row(
                    &t("agent-choose-model"),
                    Box::new(Self::bind_row_control(
                        chooser.with_enabled(can_admin),
                        provider_row(profile),
                        identity(ProviderEffect::SET_PROVIDER_MODEL)
                            .map(move |model: String| (provider.clone(), model)),
                    )),
                ));
            }
        }
    }

    fn profile_fields(&self, body: &mut Vec<Item>, profile: &ProviderProfile) {
        let location = location_shown(profile.location);
        body.push(value_row(profile.display_label(), &location));
        let profile_id = profile.id.clone();
        let fields = self.field_buffers(profile);
        let label = fields
            .label
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let endpoint = fields
            .endpoint
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let model = fields
            .model
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        body.push(field_row(
            &t("agent-placeholder-name"),
            Box::new(
                TextInput::local()
                    .with_id(&format!("agent-models-label-{profile_id}"))
                    .with_external_buffer(Arc::clone(&fields.label))
                    .with_initial_value(&label)
                    .with_placeholder(t("agent-placeholder-name")),
            ),
        ));
        body.push(field_row(
            &t("agent-placeholder-url"),
            Box::new(
                TextInput::local()
                    .with_id(&format!("agent-models-url-{profile_id}"))
                    .with_external_buffer(Arc::clone(&fields.endpoint))
                    .with_initial_value(&endpoint)
                    .with_placeholder(t("agent-placeholder-url")),
            ),
        ));
        body.push(field_row(
            &t("agent-placeholder-model"),
            Box::new(
                TextInput::local()
                    .with_id(&format!("agent-models-model-{profile_id}"))
                    .with_external_buffer(Arc::clone(&fields.model))
                    .with_initial_value(&model)
                    .with_placeholder(t("agent-placeholder-model")),
            ),
        ));
    }

    fn profile_actions(&self, body: &mut Vec<Item>, profile: &ProviderProfile) {
        let profile_id = profile.id.clone();
        let fields = self.field_buffers(profile);
        let alternate_protocol = match profile.protocol {
            ProviderProtocol::Anthropic => ProviderProtocol::OpenAiCompatible,
            ProviderProtocol::OpenAiCompatible => ProviderProtocol::Anthropic,
        };
        let alternate_protocol_label = t_args(
            "agent-use-protocol",
            &[("protocol", alternate_protocol.label().into())],
        );
        let save_id = profile_id.clone();
        let save_fields = fields;
        let read = |field: &Arc<Mutex<String>>| {
            field.lock().map(|value| value.clone()).unwrap_or_default()
        };
        body.push(row(Box::new(
            Row::new()
                .push(Box::new(self.bind_row(
                    Button::new(
                        &format!("agent-models-save-provider-{profile_id}"),
                        &t("agent-save-details"),
                    ),
                    provider_row(profile),
                    identity(ProviderEffect::SET_PROVIDER_DETAILS).map(move |()| {
                        let model = read(&save_fields.model);
                        (
                            save_id.clone(),
                            read(&save_fields.label),
                            read(&save_fields.endpoint),
                            (!model.trim().is_empty()).then_some(model),
                        )
                    }),
                )))
                .push(widgets::gap(ui::tokens::SpacingStep::S2))
                .push(Box::new(self.bind_invocation(
                    Button::new(
                        &format!("agent-models-protocol-{profile_id}"),
                        &alternate_protocol_label,
                    ),
                    ProviderEffect::SetProviderProtocol {
                        provider: profile_id.clone(),
                        protocol: protocol_slug(alternate_protocol).into(),
                    },
                )))
                .push(widgets::gap(ui::tokens::SpacingStep::S2))
                .push(Box::new(self.bind_invocation(
                    Button::new(
                        &format!("agent-models-enabled-{profile_id}"),
                        &if profile.enabled {
                            t("agent-disable")
                        } else {
                            t("agent-enable")
                        },
                    ),
                    ProviderEffect::SetProviderEnabled {
                        provider: profile_id.clone(),
                        on: !profile.enabled,
                    },
                )))
                .push(widgets::gap(ui::tokens::SpacingStep::S2))
                .push(Box::new(self.bind_invocation(
                    Button::new(
                        &format!("agent-models-remove-provider-{profile_id}"),
                        &t("agent-remove-provider"),
                    ),
                    ProviderEffect::RemoveProvider {
                        provider: profile_id,
                    },
                ))),
        )));
    }

    fn key_migration(&self, body: &mut Vec<Item>, profile: &ProviderProfile, can_admin: bool) {
        let profile_id = profile.id.clone();
        let fields = self.field_buffers(profile);
        if matches!(
            self.keys.get(&profile_id),
            Some(Ok(CredentialStatus::MigrationRequired))
        ) {
            body.push(note(&t("agent-key-migration-required")));
            let original = Arc::clone(&fields.original_endpoint);
            let initial_original = original
                .lock()
                .map(|value| value.clone())
                .unwrap_or_default();
            body.push(field_row(
                &t("agent-original-endpoint"),
                Box::new(
                    TextInput::local()
                        .with_id(&format!("agent-models-original-endpoint-{profile_id}"))
                        .with_external_buffer(Arc::clone(&original))
                        .with_initial_value(&initial_original)
                        .with_placeholder(t("agent-original-endpoint")),
                ),
            ));
            let migrate_id = profile_id.clone();
            body.push(row(Box::new(
                self.bind_row(
                    Button::new(
                        &format!("agent-models-migrate-key-{profile_id}"),
                        &t("agent-migrate-key"),
                    )
                    .with_enabled(can_admin),
                    provider_row(profile),
                    identity(ProviderEffect::MIGRATE_PROVIDER_KEY).map(move |()| {
                        let endpoint = original
                            .lock()
                            .map(|value| value.clone())
                            .unwrap_or_default();
                        (migrate_id.clone(), endpoint)
                    }),
                ),
            )));
            body.push(row(Box::new(
                self.bind_invocation(
                    Button::new(
                        &format!("agent-models-remove-legacy-key-{profile_id}"),
                        &t("agent-remove-legacy-key"),
                    )
                    .with_enabled(can_admin),
                    ProviderEffect::RemoveLegacyProviderKey {
                        provider: profile_id,
                    },
                ),
            )));
        }
    }

    fn profile_credentials(
        &self,
        body: &mut Vec<Item>,
        profile: &ProviderProfile,
        can_admin: bool,
    ) {
        let profile_id = profile.id.clone();
        if profile.sign_in.is_some() {
            body.push(note(&t("agent-sign-in-custody")));
        } else {
            let buffer = self.key_buffer(&profile_id);
            let initial = buffer.lock().map(|key| key.clone()).unwrap_or_default();
            let (configured, configured_error) = match self.keys.get(&profile_id) {
                Some(Ok(status)) => (matches!(status, CredentialStatus::Configured), None),
                Some(Err(error)) => (false, Some(error.clone())),
                None => (false, None),
            };
            if let Some(error) = configured_error {
                body.push(row(widgets::error_caption(&t_args(
                    "agent-key-read-error",
                    &[("error", error.into())],
                ))));
            }
            self.key_migration(body, profile, can_admin);
            body.push(field_row(
                &t("agent-placeholder-key"),
                Box::new(
                    TextInput::local()
                        .with_id(&format!("agent-models-api-key-{profile_id}"))
                        .with_external_buffer(buffer)
                        .with_initial_value(&initial)
                        .with_secret(true)
                        .with_placeholder(t("agent-placeholder-key")),
                ),
            ));
            body.push(row(Box::new(
                Row::new()
                    .push(Box::new(
                        self.bind_invocation(
                            Button::new(
                                &format!("agent-models-set-api-key-{profile_id}"),
                                &if configured {
                                    t("agent-replace-key")
                                } else {
                                    t("agent-save-key")
                                },
                            )
                            .with_enabled(can_admin),
                            ProviderEffect::SaveProviderKey {
                                provider: profile_id.clone(),
                                key: None,
                            },
                        ),
                    ))
                    .push(Box::new(
                        self.bind_invocation(
                            Button::new(
                                &format!("agent-models-remove-api-key-{profile_id}"),
                                &t("agent-remove-key"),
                            )
                            .with_enabled(can_admin),
                            ProviderEffect::RemoveProviderKey {
                                provider: profile_id,
                            },
                        ),
                    )),
            )));
            // why: a key-status sentence or badge is a status line, not a label/control pair.
            body.push(row(if configured {
                Box::new(Text::new(&t("agent-key-stored")).dimmed())
            } else if self.needs_key(profile) {
                semantic_status_pill(&t("agent-key-needed"), StatusTone::Warning)
            } else {
                Box::new(Text::new(&t("agent-key-none")).dimmed())
            }));
        }
    }

    fn profile_body(&self, body: &mut Vec<Item>, profile: &ProviderProfile, can_admin: bool) {
        let profile_id = profile.id.clone();
        self.profile_fields(body, profile);
        self.profile_actions(body, profile);
        let protocol = t("agent-protocol");
        body.push(value_row(&protocol, profile.protocol.label()));
        // Only a hosted provider keeps a key in this device's vault.
        if profile.location == EndpointLocation::Hosted || profile.sign_in.is_some() {
            self.profile_credentials(body, profile, can_admin);
        }
        self.profile_models(body, profile, can_admin);
        if profile.default_model.is_some() {
            body.push(row(Box::new(self.bind_invocation(
                Button::new(
                    &format!("agent-models-default-{profile_id}"),
                    &if self.provider_config().default_profile.as_deref() == Some(&profile.id) {
                        t("agent-default-current")
                    } else {
                        t("agent-use-default")
                    },
                ),
                ProviderEffect::SetDefaultProvider {
                    provider: profile_id,
                },
            ))));
        }
    }

    pub fn request_limit_buffers(
        &self,
        profile: &ProviderProfile,
        selected: &ModelDescriptor,
    ) -> LimitFieldBuffers {
        let key = (profile.id.clone(), selected.id.clone());
        let mut buffers = self.limit_field_buffers.lock_recover();
        let fields = buffers
            .entry(key)
            .or_insert_with(|| LimitFieldBuffers {
                output: Arc::new(Mutex::new(
                    selected
                        .request_limits
                        .as_ref()
                        .map_or_else(String::new, |limits| limits.max_output_tokens.to_string()),
                )),
                runtime: Arc::new(Mutex::new(
                    selected
                        .request_limits
                        .as_ref()
                        .and_then(|limits| limits.runtime_context_tokens)
                        .map_or_else(String::new, |value| value.to_string()),
                )),
                deadline: Arc::new(Mutex::new(
                    selected
                        .request_limits
                        .as_ref()
                        .and_then(|limits| limits.plan_timeout_ms)
                        .map_or_else(String::new, |value| value.to_string()),
                )),
            })
            .clone();
        drop(buffers);
        fields
    }

    /// The agents that plan with a model other than the default, each with
    /// the way back to the default. `None` while every agent uses it.
    pub fn agent_models_card(&self) -> Option<SettingsSection> {
        let config = self.provider_config();
        if config.agent_models.is_empty() {
            return None;
        }
        let mut body = vec![note(&t("agent-models-agents-caption"))];
        for agent in config.agent_models.keys() {
            let model = config.selection_for(agent).map_or_else(
                || t("agent-model-unavailable"),
                |(profile, model)| {
                    t_args(
                        "agent-model-of-provider",
                        &[
                            ("model", model.display_label().into()),
                            ("provider", profile.display_label().into()),
                        ],
                    )
                },
            );
            body.push(value_row(agent, &model));
            body.push(row(Box::new(self.bind_invocation(
                Button::new(
                    &format!("agent-model-default-{}", hex::encode(agent)),
                    &t("agent-model-use-default"),
                ),
                ProviderEffect::SetAgentModel {
                    agent: agent.clone(),
                    provider: None,
                    model: None,
                },
            ))));
        }
        Some(section(&t("agent-models-agents-title"), rows(body)))
    }

    pub fn request_limits_card(&self) -> SettingsSection {
        let config = self.provider_config().clone();
        let mut body = vec![note(&t("agent-request-limits-caption"))];
        for profile in &config.profiles {
            for selected in &profile.models {
                body.push(value_row(profile.display_label(), selected.display_label()));
                body.push(value_row(
                    &t("agent-source-context"),
                    &selected
                        .context_window
                        .map_or_else(|| t("agent-context-unknown"), |value| value.to_string()),
                ));
                let fields = self.request_limit_buffers(profile, selected);
                let widget_id =
                    format!("{}-{}", hex::encode(&profile.id), hex::encode(&selected.id));
                for (name, label, buffer) in [
                    (
                        "output",
                        "agent-output-allowance",
                        Arc::clone(&fields.output),
                    ),
                    (
                        "runtime",
                        "agent-runtime-context",
                        Arc::clone(&fields.runtime),
                    ),
                    (
                        "deadline",
                        "agent-plan-timeout",
                        Arc::clone(&fields.deadline),
                    ),
                ] {
                    let initial = buffer.lock().map(|value| value.clone()).unwrap_or_default();
                    body.push(field_row(
                        &t(label),
                        Box::new(
                            TextInput::local()
                                .with_id(&format!("agent-model-limit-{name}-{widget_id}"))
                                .with_accessible_name(t(label))
                                .with_external_buffer(buffer)
                                .with_initial_value(&initial)
                                .with_placeholder(t("agent-context-unknown")),
                        ),
                    ));
                }
                let provider = profile.id.clone();
                let model = selected.id.clone();
                body.push(row(Box::new(
                    Row::new()
                        .push(Box::new(self.bind_row(
                            Button::new(
                                &format!("agent-model-limit-save-{widget_id}"),
                                &t("agent-save-details"),
                            ),
                            request_limit_row(profile, selected),
                            identity(ProviderEffect::SET_MODEL_REQUEST_LIMITS).map(move |()| {
                                (
                                    provider.clone(),
                                    model.clone(),
                                    limit_input(&fields.output),
                                    limit_input(&fields.runtime),
                                    limit_input(&fields.deadline),
                                )
                            }),
                        )))
                        .push(Box::new(self.bind_invocation(
                            Button::new(
                                &format!("agent-model-limit-clear-{widget_id}"),
                                &t("agent-request-limits-clear"),
                            ),
                            ProviderEffect::SetModelRequestLimits {
                                provider: profile.id.clone(),
                                model: selected.id.clone(),
                                max_output_tokens: None,
                                runtime_context_tokens: None,
                                plan_timeout_ms: None,
                            },
                        ))),
                )));
            }
        }
        section(&t("agent-request-limits-title"), rows(body))
    }
}
/// Change only the selected model's policy, retaining other profile metadata.
///
/// # Errors
/// Returns an error for a missing model or limits outside its supported range.
pub fn set_model_limits(
    config: &mut ProviderConfig,
    provider: &str,
    model: &str,
    tokens: (Option<u32>, Option<u32>, Option<u32>),
) -> Result<(), String> {
    let selected = find(config, provider)?
        .models
        .iter_mut()
        .find(|item| item.id == model)
        .ok_or_else(|| t("agent-model-gone"))?;
    let limits = match tokens.0 {
        Some(max_output_tokens) => Some(ModelRequestLimits {
            max_output_tokens,
            runtime_context_tokens: tokens.1,
            plan_timeout_ms: tokens.2,
        }),
        None if tokens.1.is_none() && tokens.2.is_none() => None,
        None => return Err(t("agent-request-limits-invalid")),
    };
    if let Some(limits) = &limits {
        limits
            .validate(selected.context_window)
            .map_err(|_| t("agent-request-limits-invalid"))?;
    }
    selected.request_limits = limits;
    Ok(())
}

/// What a person can do about a failed model list.
fn listing_error(error: &ModelError) -> String {
    match error.kind {
        ErrorKind::Authentication | ErrorKind::PermissionDenied => t("agent-test-key-refused"),
        ErrorKind::Unsupported => t("agent-test-no-list"),
        ErrorKind::Transport | ErrorKind::TimedOut => t("agent-test-unreachable"),
        ErrorKind::RateLimited => t("agent-test-rate-limited"),
        ErrorKind::CreditsExhausted => t("agent-test-no-credits"),
        _ => t("agent-test-failed"),
    }
}

/// The provider `id` of `config`.
fn find<'a>(config: &'a mut ProviderConfig, id: &str) -> Result<&'a mut ProviderProfile, String> {
    config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == id)
        .ok_or_else(|| t("agent-provider-gone"))
}

fn location_shown(location: EndpointLocation) -> String {
    match location {
        EndpointLocation::Hosted => t("agent-location-hosted"),
        EndpointLocation::ThisDevice => t("agent-location-this-device"),
        EndpointLocation::PrivateNetwork => t("agent-location-private"),
    }
}

#[derive(Clone)]
pub struct HostedFieldBuffers {
    pub original_endpoint: Arc<Mutex<String>>,
    pub label: Arc<Mutex<String>>,
    pub endpoint: Arc<Mutex<String>>,
    pub model: Arc<Mutex<String>>,
}

#[derive(Clone)]
pub struct LimitFieldBuffers {
    output: Arc<Mutex<String>>,
    runtime: Arc<Mutex<String>>,
    deadline: Arc<Mutex<String>>,
}

fn limit_input(buffer: &Arc<Mutex<String>>) -> Option<u32> {
    let text = buffer
        .lock()
        .map_or_else(|_| "0".into(), |value| value.trim().to_owned());
    if text.is_empty() {
        None
    } else {
        Some(text.parse().unwrap_or(0))
    }
}

impl<H: PanelBindings> Drop for ProviderPanel<H> {
    fn drop(&mut self) {
        if let Ok(buffers) = self.provider_key_buffers.lock() {
            for buffer in buffers.values() {
                if let Ok(mut key) = buffer.lock() {
                    key.zeroize();
                }
            }
        }
    }
}
