//! Agent Models — device-local model lifecycle and provider profiles.
//!
//! Tilo owns the user-facing lifecycle, while a small adapter speaks to the
//! privileged model manager that pins Hugging Face snapshots and supervises
//! vLLM. Settings never downloads model bytes or starts a server itself.
//! Provider profiles are the person's `agent_providers` settings; their API
//! keys stay in the gateway's vault. Every change is an
//! [`AgentModelsEffect`].

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ModelRequestLimits, ProviderConfig, ProviderProfile,
    ProviderProtocol,
};
use serde::{Deserialize, Serialize};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::{self, Effect};
use ui::time::{Duration, Instant};
use ui::widget::{Button, ProgressBar, Row, Text, TextInput, Widget};
use zeroize::Zeroize;

use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, SettingsSection, StatusTone, card, card_with_action, field_row, note, row, rows,
    semantic_status_pill, value_row,
};

/// The person's agent providers, as their settings hold them: the hosted
/// ones roam with the person, those on this device or its network stay.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredProviders", into = "StoredProviders")]
pub struct AgentProviders {
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

/// Local models on this device and the person's model providers.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AgentModelsEffect {
    /// Stage the accelerator drivers the recommended model needs.
    #[effect(job, label = "Set up accelerator")]
    SetUpAccelerator,
    /// Show or hide what downloading the recommended model involves.
    #[effect(view)]
    ReviewModelDownload { show: bool },
    /// Show or hide what downloading one in-process model involves.
    #[effect(view, row)]
    ReviewLocalModel {
        #[param("The model's id")]
        model: String,
        show: bool,
    },
    /// Show one signed in-process model in Agent Models, first, at its
    /// licence confirmation when it is not installed yet. Installs nothing.
    #[effect(view, label = "Show local model")]
    ShowLocalModel {
        #[param("Signed model id, for example u2net-subject")]
        model: String,
    },
    /// Download and install the recommended model or an in-process one,
    /// accepting its license.
    #[effect(job, label = "Install model")]
    InstallModel {
        #[param("The model's id")]
        model: String,
        #[param("The pinned revision")]
        revision: String,
    },
    /// Start serving an installed model.
    #[effect(job, row, idempotent, label = "Start model")]
    StartModel {
        #[param("The model's id")]
        model: String,
    },
    /// Stop serving a model.
    #[effect(job, row, idempotent, label = "Stop model")]
    StopModel {
        #[param("The model's id")]
        model: String,
    },
    /// Install a model again after it failed.
    #[effect(job, row, label = "Retry")]
    RetryModel {
        #[param("The model's id")]
        model: String,
        #[param("The pinned revision")]
        revision: String,
    },
    /// Remove an installed model and its files.
    #[effect(job, row, destructive, label = "Remove model")]
    RemoveModel {
        #[param("The model's id")]
        model: String,
    },
    /// Add a hosted provider speaking `anthropic` or `openai-compatible`.
    #[effect(label = "Add provider")]
    AddProvider {
        #[param("`anthropic` or `openai-compatible`")]
        protocol: String,
    },
    /// Speak another protocol to a provider.
    #[effect(row, idempotent)]
    SetProviderProtocol {
        #[param("The provider's id")]
        provider: String,
        #[param("`anthropic` or `openai-compatible`")]
        protocol: String,
    },
    /// Offer a provider to agents, or stop offering it.
    #[effect(row, idempotent)]
    SetProviderEnabled {
        #[param("The provider's id")]
        provider: String,
        on: bool,
    },
    /// Name a provider, where it is, and the model agents use from it.
    #[effect(row)]
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
    #[effect(row, idempotent)]
    SetModelRequestLimits {
        provider: String,
        model: String,
        max_output_tokens: Option<u32>,
        runtime_context_tokens: Option<u32>,
        plan_timeout_ms: Option<u32>,
    },
    /// Read source context and this model's configured deployment limits.
    #[effect(row, returns, permission = "read")]
    InspectModelRequestLimits { provider: String, model: String },
    /// Make a provider the one agents use unless told otherwise.
    #[effect(row, idempotent, label = "Use by default")]
    SetDefaultProvider {
        #[param("The provider's id")]
        provider: String,
    },
    /// Remove a provider and its stored API key.
    #[effect(row, destructive, label = "Remove provider")]
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

fn token(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// The effect `action` is for `model`.
fn model_effect(model: &InstalledLocalModel, action: ModelAction) -> AgentModelsEffect {
    let id = model.package.id.clone();
    match action {
        ModelAction::Start => AgentModelsEffect::StartModel { model: id },
        ModelAction::Stop => AgentModelsEffect::StopModel { model: id },
        ModelAction::Retry => AgentModelsEffect::RetryModel {
            model: id,
            revision: model.package.revision.clone(),
        },
        ModelAction::Remove => AgentModelsEffect::RemoveModel { model: id },
    }
}

/// Answer `effect`: the manager's work runs on a worker, then the models
/// are read again; provider changes write the person's settings.
#[allow(
    clippy::too_many_lines,
    reason = "one match arm per effect of the area, each answered where it is read"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: AgentModelsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let shared = app.agent_models().model();
    let mut model = shared.borrow_mut();
    model.changed = true;
    match effect {
        AgentModelsEffect::InspectModelRequestLimits {
            provider,
            model: id,
        } => {
            let config = model.providers.get().config.clone();
            let selected = config
                .profiles
                .iter()
                .find(|item| item.id == provider)
                .and_then(|profile| profile.models.iter().find(|item| item.id == id))
                .ok_or_else(|| t("agent-model-gone"))?;
            Ok(Effected::value(serde_json::json!({
                "provider":provider, "model":id,
                "source_context_tokens":selected.context_window,
                "request_limits":selected.request_limits,
                "plan_timeout_ceiling_ms":selected.request_limits.as_ref()
                    .and_then(|limits| limits.plan_timeout_ms).unwrap_or(60_000),
                "caller_may_request_shorter_deadline":true,
                "runtime_context_attested":false, "prompt_fit_proven":false
            })))
        }
        AgentModelsEffect::ReviewModelDownload { show } => {
            model.reviewing.set(show);
            Ok(Effected::done())
        }
        AgentModelsEffect::ReviewLocalModel { model: id, show } => {
            let installable = model
                .snapshot
                .in_process_model(&id)
                .is_some_and(InProcessLocalModel::installable);
            if show && !installable {
                return Err(t("agent-notice-inprocess-changed"));
            }
            model.reviewing_local.set(show.then_some(id));
            Ok(Effected::done())
        }
        AgentModelsEffect::ShowLocalModel { model: id } => {
            drop(model);
            // The pane switches at the next sync, as a sidebar pick does.
            app.request_pane(crate::categories::Category::AgentModels);
            let shared = app.agent_models().model();
            let mut model = shared.borrow_mut();
            model.reveal(&id)?;
            Ok(Effected::done())
        }
        AgentModelsEffect::SetUpAccelerator
        | AgentModelsEffect::InstallModel { .. }
        | AgentModelsEffect::StartModel { .. }
        | AgentModelsEffect::StopModel { .. }
        | AgentModelsEffect::RetryModel { .. }
        | AgentModelsEffect::RemoveModel { .. } => {
            let work = model.manager_work(&effect)?;
            let control = Arc::clone(&model.control);
            if matches!(effect, AgentModelsEffect::InstallModel { .. }) {
                model.reviewing.set(false);
                model.reviewing_local.set(None);
            }
            drop(model);
            Ok(cx.thread(
                move |_| work.run(control.as_ref()),
                |app: &mut SettingsApp, outcome: Result<String, String>, _| {
                    let model = app.agent_models().model();
                    let mut model = model.borrow_mut();
                    model.refresh();
                    if let Err(error) = &outcome {
                        model.notice = Some(error.clone());
                    }
                    outcome.map(|_| Effected::done())
                },
            ))
        }
        AgentModelsEffect::SaveProviderKey { provider, key } => {
            let buffer = model.key_buffer(&provider);
            drop(model);
            Ok(cx.thread(
                move |_| {
                    let mut typed = buffer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let value = key.unwrap_or_else(|| std::mem::take(&mut *typed));
                    typed.zeroize();
                    drop(typed);
                    if value.trim().is_empty() {
                        return Err(t_args(
                            "agent-key-save-err",
                            &[("error", "the key is empty".into())],
                        ));
                    }
                    tilo_model_gateway::set_api_key(&provider, value).map_err(|error| {
                        t_args("agent-key-save-err", &[("error", error.to_string().into())])
                    })
                },
                |app: &mut SettingsApp, outcome: Result<(), String>, _| {
                    let model = app.agent_models().model();
                    let mut model = model.borrow_mut();
                    model.read_keys();
                    model.provider_notice(outcome.as_ref().map(|()| t("agent-key-saved")));
                    outcome.map(|()| Effected::done())
                },
            ))
        }
        AgentModelsEffect::RemoveProviderKey { provider } => {
            drop(model);
            Ok(cx.thread(
                move |_| {
                    tilo_model_gateway::remove_api_key(&provider).map_err(|error| {
                        t_args(
                            "agent-key-remove-err",
                            &[("error", error.to_string().into())],
                        )
                    })
                },
                |app: &mut SettingsApp, outcome: Result<(), String>, _| {
                    let model = app.agent_models().model();
                    let mut model = model.borrow_mut();
                    model.read_keys();
                    model.provider_notice(outcome.as_ref().map(|()| t("agent-key-removed")));
                    outcome.map(|()| Effected::done())
                },
            ))
        }
        provider => {
            let result = model.change_providers(provider);
            model.provider_notice(result.as_ref().map(|()| t("agent-provider-saved")));
            result.map(|()| Effected::done())
        }
    }
}

/// One call to the model manager, checked against what it reported.
enum ManagerWork {
    Accelerator,
    Install(LocalModelPackage),
    Start(String),
    Stop(String),
    Remove(String),
}

impl ManagerWork {
    /// Carry the call out. Never on the UI thread: the manager may take
    /// seconds.
    fn run(&self, control: &dyn LocalModelControl) -> Result<String, String> {
        match self {
            Self::Accelerator => control
                .stage_accelerator()
                .map(|()| t("agent-notice-accel-ok"))
                .map_err(|error| t_args("agent-notice-accel-err", &[("error", error.into())])),
            Self::Install(package) => control
                .install(package)
                .map(|()| t("agent-notice-install-ok"))
                .map_err(|error| t_args("agent-notice-install-err", &[("error", error.into())])),
            Self::Start(id) => Self::named(ModelAction::Start, control.start(id)),
            Self::Stop(id) => Self::named(ModelAction::Stop, control.stop(id)),
            Self::Remove(id) => Self::named(ModelAction::Remove, control.remove(id)),
        }
    }

    fn named(action: ModelAction, result: Result<(), String>) -> Result<String, String> {
        result
            .map(|()| {
                t_args(
                    "agent-notice-action-ok",
                    &[("action", action_label(action).into())],
                )
            })
            .map_err(|error| {
                t_args(
                    "agent-notice-action-failed",
                    &[
                        ("action", action_label(action).into()),
                        ("error", error.into()),
                    ],
                )
            })
    }
}

/// What the agent models area keeps for the window: the model, asked of
/// the manager when first needed.
pub struct AgentModelsArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<AgentModelsModel>>,
    /// The manager's snapshot and the hosted keys, asked off the UI thread
    /// after the window's first frame ([`Self::start_read`]).
    read: crate::device_read::DeviceRead<AgentModelsRead>,
}

/// What the agent models area asks of the device: the model manager's
/// snapshot, and whether this device's vault holds the key of each hosted
/// provider configured when it asked. Asked off the UI thread; the model is
/// built from it on the UI thread ([`AgentModelsModel::from_read`]).
pub(crate) struct AgentModelsRead {
    snapshot: Result<LocalModelSnapshot, String>,
    keys: HashMap<String, Result<bool, String>>,
}

impl AgentModelsRead {
    /// Ask `control` for its snapshot and the gateway for the keys of the
    /// `hosted` providers.
    fn of(control: &dyn LocalModelControl, hosted: Vec<String>) -> Self {
        Self {
            snapshot: control.snapshot(),
            keys: hosted_keys(hosted),
        }
    }
}

/// The ids of the hosted providers in `providers`, whose keys this
/// device's vault holds or not.
fn hosted_profiles(providers: &AgentProviders) -> Vec<String> {
    providers
        .config
        .profiles
        .iter()
        .filter(|profile| profile.location == EndpointLocation::Hosted)
        .map(|profile| profile.id.clone())
        .collect()
}

/// Ask the gateway whether this device's vault holds each of `hosted`'s
/// keys: one blocking request each.
fn hosted_keys(hosted: Vec<String>) -> HashMap<String, Result<bool, String>> {
    hosted
        .into_iter()
        .map(|id| {
            let configured =
                tilo_model_gateway::secret_configured(&id).map_err(|error| error.to_string());
            (id, configured)
        })
        .collect()
}

fn request_limit_rows(profile: &ProviderProfile) -> Vec<LiveRow> {
    let mut result = Vec::new();
    let offer = |effect: AgentModelsEffect| SettingsEffect::from(effect).bound();
    for selected in &profile.models {
        result.push(
            LiveRow::new(
                format!(
                    "settings.agent-provider-model:{}:{}",
                    hex::encode(&profile.id),
                    hex::encode(&selected.id)
                ),
                "settings.agent-provider-model",
                selected.display_label().to_owned(),
            )
            .operation(offer(AgentModelsEffect::InspectModelRequestLimits {
                provider: profile.id.clone(),
                model: selected.id.clone(),
            }))
            .operation(
                SettingsEffect::from(AgentModelsEffect::SetModelRequestLimits {
                    provider: profile.id.clone(),
                    model: selected.id.clone(),
                    max_output_tokens: None,
                    runtime_context_tokens: None,
                    plan_timeout_ms: None,
                })
                .bound_asking(&[
                    "max_output_tokens",
                    "runtime_context_tokens",
                    "plan_timeout_ms",
                ]),
            ),
        );
    }
    result
}

impl AgentModelsArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
            read: crate::device_read::DeviceRead::default(),
        }
    }

    /// The model manager: this device's, or a sample one for a screenshot.
    fn control(&self) -> Arc<dyn LocalModelControl> {
        if self.cx.reaches_devices() {
            Arc::new(ModelManagerCli)
        } else {
            Arc::new(SampleManager)
        }
    }

    /// The model, over the model manager, or over a sample one for a
    /// screenshot: from the read [`Self::start_read`] started, waiting for
    /// it if it still runs, else asked here.
    pub fn model(&self) -> &Shared<AgentModelsModel> {
        self.model.get_or_init(|| {
            let control = self.control();
            let model = match self.read.take() {
                Some(read) => AgentModelsModel::from_read(self.cx.clone(), control, read),
                None => AgentModelsModel::new(self.cx.clone(), control),
            };
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// Ask the model manager and the gateway off the UI thread; `false`
    /// when they have been asked or are being asked.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let control = self.control();
        let hosted = hosted_profiles(self.cx.follow::<AgentProviders>().get());
        self.read
            .start(move || AgentModelsRead::of(control.as_ref(), hosted))
    }

    /// Whether the read [`Self::start_read`] started has ended.
    pub(crate) fn read_finished(&self) -> bool {
        self.read.is_finished()
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::effects::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// One row per installed model, per provider, and the recommendation.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let offer = |effect: AgentModelsEffect| SettingsEffect::from(effect).bound();
        let mut rows: Vec<LiveRow> = model
            .snapshot
            .models
            .iter()
            .map(|installed| {
                available_actions(&installed.phase).into_iter().fold(
                    LiveRow::new(
                        format!("settings.agent-model:{}", installed.package.id),
                        "settings.agent-model",
                        installed.package.name.clone(),
                    ),
                    |row, action| row.operation(offer(model_effect(installed, action))),
                )
            })
            .collect();
        // In-process models run inside the apps that use them on any CPU, so
        // neither the hardware fit nor accelerator setup gates them.
        for local in &model.snapshot.in_process {
            let mut row = LiveRow::new(
                format!("settings.agent-model:{}", local.package.id),
                "settings.agent-model-in-process",
                local.package.name.clone(),
            );
            if local.installable() {
                row = row.operation(offer(AgentModelsEffect::InstallModel {
                    model: local.package.id.clone(),
                    revision: local.package.revision.clone(),
                }));
            }
            if local.removable() {
                row = row.operation(offer(AgentModelsEffect::RemoveModel {
                    model: local.package.id.clone(),
                }));
            }
            rows.push(row);
        }
        if let Some(package) = &model.snapshot.recommendation {
            rows.push(
                LiveRow::new(
                    format!("settings.agent-model-recommendation:{}", package.id),
                    "settings.agent-model-recommendation",
                    package.name.clone(),
                )
                .operation(offer(AgentModelsEffect::InstallModel {
                    model: package.id.clone(),
                    revision: package.revision.clone(),
                })),
            );
        }
        for profile in &model.providers.get().config.profiles {
            let id = profile.id.clone();
            let row = LiveRow::new(
                format!("settings.agent-provider:{id}"),
                "settings.agent-provider",
                profile.display_label().to_owned(),
            )
            .operation(offer(AgentModelsEffect::SetProviderEnabled {
                provider: id.clone(),
                on: !profile.enabled,
            }))
            .operation(offer(AgentModelsEffect::SetDefaultProvider {
                provider: id.clone(),
            }))
            .operation(offer(AgentModelsEffect::RemoveProvider {
                provider: id.clone(),
            }));
            let row = if model.needs_key(profile) {
                row.operation(
                    SettingsEffect::from(AgentModelsEffect::SaveProviderKey {
                        provider: id,
                        key: None,
                    })
                    .bound_asking(&["key"]),
                )
            } else {
                row
            };
            rows.push(row);
            rows.extend(request_limit_rows(profile));
        }
        rows
    }
}

pub struct AgentModelsModel {
    cx: PaneCx,
    control: Arc<dyn LocalModelControl>,
    snapshot: LocalModelSnapshot,
    manager_available: bool,
    /// Whether the local provider profile follows what the manager runs.
    follow_local_profile: bool,
    providers: Followed<AgentProviders>,
    /// Whether the download of the recommended model is being reviewed.
    reviewing: LocalState<bool>,
    /// The in-process model whose download is being reviewed.
    reviewing_local: LocalState<Option<String>>,
    /// The in-process model a link asked to show; its card comes first.
    revealed: Option<String>,
    notice: Option<String>,
    last_poll: Instant,
    /// API keys are deliberately process-local masked-input buffers. They
    /// never enter the settings, and are erased as soon as the gateway
    /// accepts or rejects them.
    provider_key_buffers: Mutex<HashMap<String, Arc<Mutex<String>>>>,
    provider_field_buffers: Mutex<HashMap<String, HostedFieldBuffers>>,
    limit_field_buffers: Mutex<HashMap<(String, String), LimitFieldBuffers>>,
    /// Whether this device's vault holds each hosted provider's key, or why
    /// that could not be told.
    keys: HashMap<String, Result<bool, String>>,
    changed: bool,
}

impl AgentModelsModel {
    /// The model over `control`, asking it and the gateway here.
    fn new(cx: PaneCx, control: Arc<dyn LocalModelControl>) -> Self {
        let hosted = hosted_profiles(cx.follow::<AgentProviders>().get());
        let read = AgentModelsRead::of(control.as_ref(), hosted);
        Self::from_read(cx, control, read)
    }

    /// The model over `control`, from what was asked of it and of the
    /// gateway. Keys asked for a different set of hosted providers than the
    /// person's settings now hold are asked again.
    fn from_read(cx: PaneCx, control: Arc<dyn LocalModelControl>, read: AgentModelsRead) -> Self {
        let (snapshot, manager_available, notice) = match read.snapshot {
            Ok(snapshot) => (snapshot, true, None),
            Err(error) => (
                fallback_snapshot(),
                false,
                Some(t_args(
                    "agent-notice-service-down",
                    &[("error", error.into())],
                )),
            ),
        };
        let mut model = Self {
            providers: cx.follow::<AgentProviders>(),
            cx,
            control,
            snapshot,
            manager_available,
            follow_local_profile: true,
            reviewing: LocalState::new(false),
            reviewing_local: LocalState::new(None),
            revealed: None,
            notice,
            last_poll: ui::time::now(),
            provider_key_buffers: Mutex::new(HashMap::new()),
            provider_field_buffers: Mutex::new(HashMap::new()),
            limit_field_buffers: Mutex::new(HashMap::new()),
            keys: HashMap::new(),
            changed: false,
        };
        model.sync_local_profile();
        let hosted = hosted_profiles(model.providers.get());
        if hosted.len() == read.keys.len() && hosted.iter().all(|id| read.keys.contains_key(id)) {
            model.keys = read.keys;
            model.changed = true;
        } else {
            model.read_keys();
        }
        model
    }

    /// Ask the gateway whether this device's vault holds each hosted
    /// provider's key.
    fn read_keys(&mut self) {
        self.keys = hosted_keys(hosted_profiles(self.providers.get()));
        self.changed = true;
    }

    /// Whether `profile` is hosted and this device's vault has no key for
    /// it: one that roamed here from another device needs its own.
    fn needs_key(&self, profile: &ProviderProfile) -> bool {
        profile.location == EndpointLocation::Hosted
            && matches!(self.keys.get(&profile.id), Some(Ok(false)))
    }

    fn refresh(&mut self) {
        match self.control.snapshot() {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.manager_available = true;
                self.notice = None;
                self.sync_local_profile();
            }
            Err(error) => {
                self.manager_available = false;
                self.notice = Some(t_args(
                    "agent-notice-refresh-err",
                    &[("error", error.into())],
                ));
            }
        }
        self.last_poll = ui::time::now();
        self.changed = true;
    }

    /// Keep the local provider profile on what the manager serves.
    fn sync_local_profile(&mut self) {
        if !self.follow_local_profile {
            return;
        }
        let config = &self.providers.get().config;
        let existing = config
            .profiles
            .iter()
            .find(|profile| profile.is_local_vllm_candidate())
            .cloned();
        let Some(profile) = synced_local_profile(existing.clone(), &self.snapshot.models) else {
            return;
        };
        if existing.as_ref() != Some(&profile) {
            let _ = self.providers.settings().update(|providers| {
                providers.config.upsert(profile);
            });
            self.providers.refresh();
        }
    }

    /// Show one in-process model's card first, opened at its licence
    /// confirmation when it is not installed yet. Nothing is downloaded
    /// until the person accepts there.
    fn reveal(&mut self, id: &str) -> Result<(), String> {
        self.changed = true;
        let Some(model) = self.snapshot.in_process_model(id) else {
            let unknown = t_args(
                "agent-notice-unknown-model",
                &[("id", id.to_owned().into())],
            );
            self.notice = Some(unknown.clone());
            return Err(unknown);
        };
        if model.installable() {
            self.reviewing_local.set(Some(model.package.id.clone()));
        }
        self.revealed = Some(model.package.id.clone());
        Ok(())
    }

    fn is_busy(&self) -> bool {
        self.snapshot
            .models
            .iter()
            .any(|model| model.phase.is_transient())
            || self.snapshot.in_process.iter().any(|model| {
                model
                    .phase
                    .as_ref()
                    .is_some_and(LocalModelPhase::is_transient)
            })
            || self
                .snapshot
                .accelerator_setup
                .as_ref()
                .is_some_and(|setup| setup.status.is_transient())
    }

    /// The manager call `effect` asks for, revalidated against the models
    /// the manager last reported.
    fn manager_work(&self, effect: &AgentModelsEffect) -> Result<ManagerWork, String> {
        let installed = |id: &str| {
            self.snapshot
                .models
                .iter()
                .find(|model| model.package.id == id)
                .ok_or_else(|| t("agent-model-gone"))
        };
        let allowed = |model: &InstalledLocalModel, action: ModelAction| {
            if available_actions(&model.phase).contains(&action) {
                Ok(())
            } else {
                Err(t("agent-model-busy"))
            }
        };
        match effect {
            AgentModelsEffect::SetUpAccelerator => Ok(ManagerWork::Accelerator),
            AgentModelsEffect::InstallModel { model, revision } => {
                if let Some(local) = self.snapshot.in_process_model(model) {
                    // The action's label is the licence acceptance; it holds
                    // only for the revision it was offered with.
                    return if &local.package.revision == revision && local.installable() {
                        Ok(ManagerWork::Install(local.package.clone()))
                    } else {
                        Err(t("agent-notice-inprocess-changed"))
                    };
                }
                self.snapshot
                    .recommendation
                    .clone()
                    .filter(|package| &package.id == model && &package.revision == revision)
                    .map(ManagerWork::Install)
                    .ok_or_else(|| t("agent-model-gone"))
            }
            AgentModelsEffect::StartModel { model } => {
                let model = installed(model)?;
                allowed(model, ModelAction::Start)?;
                Ok(ManagerWork::Start(model.package.id.clone()))
            }
            AgentModelsEffect::StopModel { model } => {
                let model = installed(model)?;
                allowed(model, ModelAction::Stop)?;
                Ok(ManagerWork::Stop(model.package.id.clone()))
            }
            AgentModelsEffect::RetryModel { model, revision } => {
                let model = installed(model)?;
                allowed(model, ModelAction::Retry)?;
                if &model.package.revision != revision {
                    return Err(t("agent-model-gone"));
                }
                Ok(ManagerWork::Install(model.package.clone()))
            }
            AgentModelsEffect::RemoveModel { model } => {
                if let Some(local) = self.snapshot.in_process_model(model) {
                    return if local.removable() {
                        Ok(ManagerWork::Remove(local.package.id.clone()))
                    } else {
                        Err(t("agent-notice-inprocess-changed"))
                    };
                }
                let model = installed(model)?;
                allowed(model, ModelAction::Remove)?;
                Ok(ManagerWork::Remove(model.package.id.clone()))
            }
            _ => Err("not a model manager effect".into()),
        }
    }

    /// Apply a provider change to the person's settings.
    fn change_providers(&mut self, effect: AgentModelsEffect) -> Result<(), String> {
        let mut config = self.providers.get().config.clone();
        match effect {
            AgentModelsEffect::SetModelRequestLimits {
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
                self.limit_field_buffers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&(provider, model));
            }
            AgentModelsEffect::AddProvider { protocol: slug } => {
                config.upsert(ProviderProfile::hosted(protocol(&slug)?));
            }
            AgentModelsEffect::SetProviderProtocol {
                provider,
                protocol: slug,
            } => {
                find(&mut config, &provider)?.protocol = protocol(&slug)?;
            }
            AgentModelsEffect::SetProviderEnabled { provider, on } => {
                find(&mut config, &provider)?.enabled = on;
            }
            AgentModelsEffect::SetDefaultProvider { provider } => {
                let available = config.profiles.iter().any(|profile| {
                    profile.id == provider && profile.enabled && profile.default_model.is_some()
                });
                if !available {
                    return Err(t("agent-provider-not-default"));
                }
                config.default_profile = Some(provider);
            }
            AgentModelsEffect::SetProviderDetails {
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
            AgentModelsEffect::RemoveProvider { provider } => {
                if config.remove(&provider).is_none() {
                    return Err(t("agent-provider-gone"));
                }
                tilo_model_gateway::remove_api_key(&provider).map_err(|error| {
                    t_args(
                        "agent-key-remove-err",
                        &[("error", error.to_string().into())],
                    )
                })?;
            }
            _ => return Err("not a provider effect".into()),
        }
        let config = config.sanitised();
        self.providers
            .settings()
            .update(|providers| providers.config = config)
            .map_err(|error| {
                t_args(
                    "agent-provider-save-err",
                    &[("error", error.to_string().into())],
                )
            })?;
        self.providers.refresh();
        self.changed = true;
        Ok(())
    }

    fn provider_notice(&mut self, outcome: Result<String, &String>) {
        self.notice = Some(match outcome {
            Ok(message) => message,
            Err(error) => error.clone(),
        });
        self.changed = true;
    }

    fn accelerator_card(&self) -> Option<SettingsSection> {
        let setup = self.snapshot.accelerator_setup.as_ref()?;
        let match_label = t("agent-accel-match");
        let mut body = vec![value_row(&match_label, &setup.boot_pack_id)];
        let (status, tone) = match &setup.status {
            AcceleratorSetupState::Available => {
                body.push(note(&t("agent-accel-available")));
                body.push(row(Box::new(Row::new().push(Box::new(
                    self.cx.bind_invocation(
                        Button::new("agent-models-accelerator-setup", &t("agent-accel-setup")),
                        AgentModelsEffect::SetUpAccelerator,
                    ),
                )))));
                (t("agent-accel-ready"), StatusTone::Neutral)
            }
            AcceleratorSetupState::Queued => {
                body.push(row(Box::new(
                    ProgressBar::indeterminate().with_label(t("agent-accel-preparing")),
                )));
                body.push(note(&t("agent-accel-queued-caption")));
                (t("agent-accel-queued"), StatusTone::Neutral)
            }
            AcceleratorSetupState::Staging => {
                body.push(row(Box::new(
                    ProgressBar::indeterminate().with_label(t("agent-accel-staging-label")),
                )));
                body.push(note(&t("agent-accel-staging-caption")));
                (t("agent-accel-staging"), StatusTone::Warning)
            }
            AcceleratorSetupState::RestartRequired => {
                body.push(note(&t("agent-accel-restart-caption")));
                (t("agent-accel-restart"), StatusTone::Warning)
            }
            AcceleratorSetupState::Active => {
                body.push(note(&t("agent-accel-active-caption")));
                (t("agent-accel-active"), StatusTone::Success)
            }
            AcceleratorSetupState::Error { message } => {
                body.push(note(message));
                body.push(note(&t("agent-accel-error-retry")));
                body.push(row(Box::new(Row::new().push(Box::new(
                    self.cx.bind_invocation(
                        Button::new("agent-models-accelerator-retry", &t("agent-accel-retry")),
                        AgentModelsEffect::SetUpAccelerator,
                    ),
                )))));
                (t("agent-status-attention"), StatusTone::Warning)
            }
        };
        Some(card_with_action(
            &t("agent-accel-title"),
            semantic_status_pill(&status, tone),
            rows(body),
        ))
    }

    fn push_recommendation_confirm(&self, body: &mut Vec<Item>, package: &LocalModelPackage) {
        self.push_confirm(
            body,
            package,
            (
                "agent-models-confirm-install",
                "agent-models-cancel-install",
            ),
            AgentModelsEffect::ReviewModelDownload { show: false },
        );
    }

    /// The download, storage, source, revision and licence a person accepts,
    /// with the accept and cancel controls (widget ids `controls`); cancel
    /// invokes `cancel`.
    fn push_confirm(
        &self,
        body: &mut Vec<Item>,
        package: &LocalModelPackage,
        controls: (&str, &str),
        cancel_effect: AgentModelsEffect,
    ) {
        let download = t("agent-download");
        let download_size = byte_size(package.download_bytes);
        let after = t("agent-storage-after");
        let after_size = byte_size(package.installed_bytes);
        let source = t("agent-source");
        let revision = t("agent-revision");
        let license = t("agent-license");
        let accept = t_args(
            "agent-license-accept",
            &[("url", package.license_url.as_str().into())],
        );
        let accept_btn = t("agent-accept-download");
        let cancel = t("agent-cancel");
        body.extend([
            value_row(&download, &download_size),
            value_row(&after, &after_size),
            value_row(&source, &package.source),
            value_row(&revision, &package.revision),
            value_row(&license, &package.license),
            note(&accept),
            row(Box::new(
                Row::new()
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(controls.0, &accept_btn),
                        AgentModelsEffect::InstallModel {
                            model: package.id.clone(),
                            revision: package.revision.clone(),
                        },
                    )))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(controls.1, &cancel),
                        cancel_effect,
                    ))),
            )),
        ]);
    }

    fn recommendation_card(&self) -> SettingsSection {
        let hardware = &self.snapshot.hardware;
        let fit_label = hardware.fit.label();
        let mem = gib(i64::from(hardware.memory_gib));
        let hw_fit = t("agent-hw-fit");
        let hw_mem = t("agent-hw-memory");
        let hw_accel = t("agent-hw-accel");
        let mut body = vec![
            value_row(&hw_fit, &fit_label),
            value_row(&hw_mem, &mem),
            value_row(&hw_accel, &hardware.accelerator),
        ];
        if let Some(vram) = hardware.accelerator_memory_gib {
            let label = t("agent-hw-accel-mem");
            let value = gib(i64::from(vram));
            body.push(value_row(&label, &value));
        }
        if let Some(storage) = hardware.free_storage_gib {
            let label = t("agent-hw-storage");
            let value = gib(i64::from(storage));
            body.push(value_row(&label, &value));
        }
        body.push(note(&hardware.reason));
        let Some(package) = &self.snapshot.recommendation else {
            let rec_model = t("agent-rec-model");
            let waiting = t("agent-rec-waiting");
            body.push(value_row(&rec_model, &waiting));
            body.push(note(&t("agent-rec-waiting-caption")));
            let unavailable = t("agent-setup-unavailable");
            body.push(row(Box::new(Row::new().push(Box::new(
                Button::new("agent-models-setup-unavailable", &unavailable).with_enabled(false),
            )))));
            return card_with_action(
                &t("agent-rec-title"),
                semantic_status_pill(&t("agent-service-unavailable"), StatusTone::Warning),
                rows(body),
            );
        };
        let rec_model = t("agent-rec-model");
        body.extend([
            value_row(&rec_model, &package.name),
            note(&package.description),
        ]);
        if self.reviewing.with(|reviewing| *reviewing) {
            self.push_recommendation_confirm(&mut body, package);
        } else {
            let setup = t("agent-setup-device");
            body.push(row(Box::new(
                Row::new().push(Box::new(
                    self.cx.bind_invocation(
                        Button::new("agent-models-setup-local", &setup).with_enabled(
                            self.manager_available
                                && hardware.fit != HardwareFit::Unsupported
                                && self
                                    .snapshot
                                    .accelerator_setup
                                    .as_ref()
                                    .is_none_or(|setup| !setup.status.blocks_model_setup()),
                        ),
                        AgentModelsEffect::ReviewModelDownload { show: true },
                    ),
                )),
            )));
        }
        card_with_action(
            &t("agent-rec-title"),
            semantic_status_pill(&fit_label, fit_tone(hardware.fit)),
            rows(body),
        )
    }

    /// One in-process model: what it does, then setup with the same licence
    /// confirmation as the recommendation, progress, or ready with Remove.
    /// Never Start or Stop: it runs inside the apps that use it.
    #[allow(
        clippy::too_many_lines,
        reason = "every install phase of the card stays beside its controls"
    )]
    fn in_process_card(&self, model: &InProcessLocalModel) -> SettingsSection {
        let package = &model.package;
        let widget_token = token(&package.id);
        let mut body = vec![
            value_row(&t("agent-inprocess-model"), &package.name),
            note(&package.description),
            note(&t("agent-inprocess-caption")),
        ];
        let confirming = self
            .reviewing_local
            .with(|reviewing| reviewing.as_deref() == Some(package.id.as_str()));
        if confirming && model.installable() {
            let confirm_id = format!("agent-models-inprocess-confirm-{widget_token}");
            let cancel_id = format!("agent-models-inprocess-cancel-{widget_token}");
            self.push_confirm(
                &mut body,
                package,
                (&confirm_id, &cancel_id),
                AgentModelsEffect::ReviewLocalModel {
                    model: package.id.clone(),
                    show: false,
                },
            );
        } else {
            match &model.phase {
                None => {
                    body.push(value_row(
                        &t("agent-download"),
                        &byte_size(package.download_bytes),
                    ));
                }
                Some(
                    phase @ LocalModelPhase::Downloading {
                        downloaded_bytes,
                        total_bytes,
                    },
                ) => {
                    let progress = t_args(
                        "agent-progress-downloading",
                        &[("name", package.name.as_str().into())],
                    );
                    body.push(row(Box::new(
                        ProgressBar::new(phase_progress(phase).unwrap_or(0.0)).with_label(progress),
                    )));
                    let done = byte_size(*downloaded_bytes);
                    let total = byte_size(*total_bytes);
                    body.push(note(&t_args(
                        "agent-download-progress",
                        &[("done", done.as_str().into()), ("total", total.into())],
                    )));
                }
                Some(LocalModelPhase::Installing { message, .. }) => {
                    let progress = t_args(
                        "agent-progress-installing",
                        &[("name", package.name.as_str().into())],
                    );
                    body.push(row(Box::new(
                        ProgressBar::indeterminate().with_label(progress),
                    )));
                    body.push(note(message));
                }
                Some(LocalModelPhase::Ready { .. }) => {
                    body.push(value_row(
                        &t("agent-model-storage"),
                        &byte_size(package.installed_bytes),
                    ));
                    body.push(value_row(&t("agent-license"), &package.license));
                    body.push(note(&t("agent-inprocess-ready-caption")));
                }
                Some(LocalModelPhase::Error { message }) => {
                    body.push(note(message));
                    body.push(note(&t("agent-inprocess-error-caption")));
                }
            }
            let mut buttons = Row::new();
            let mut any = false;
            if model.installable() {
                buttons = buttons.push(Box::new(
                    self.cx.bind_invocation(
                        Button::new(
                            &format!("agent-models-inprocess-setup-{widget_token}"),
                            &t("agent-setup-device"),
                        )
                        .with_enabled(self.manager_available),
                        AgentModelsEffect::ReviewLocalModel {
                            model: package.id.clone(),
                            show: true,
                        },
                    ),
                ));
                any = true;
            }
            if model.removable() {
                buttons = buttons.push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("agent-models-inprocess-remove-{widget_token}"),
                        &action_label(ModelAction::Remove),
                    ),
                    AgentModelsEffect::RemoveModel {
                        model: package.id.clone(),
                    },
                )));
                any = true;
            }
            if any {
                body.push(row(Box::new(buttons)));
            }
        }
        let (status, tone) = match &model.phase {
            None => (t("agent-status-not-installed"), StatusTone::Neutral),
            Some(LocalModelPhase::Ready { .. }) => (t("agent-status-ready"), StatusTone::Success),
            Some(phase) => (phase.status(), phase_tone(phase)),
        };
        card_with_action(
            &package.capability_title(),
            semantic_status_pill(&status, tone),
            rows(body),
        )
    }

    fn model_card(&self, model: &InstalledLocalModel) -> SettingsSection {
        let status = model.phase.status();
        let storage = byte_size(model.package.installed_bytes);
        let status_label = t("agent-model-status");
        let storage_label = t("agent-model-storage");
        let license_label = t("agent-license");
        let mut body = vec![
            value_row(&status_label, &status),
            value_row(&storage_label, &storage),
            value_row(&license_label, &model.package.license),
        ];
        match &model.phase {
            LocalModelPhase::Downloading {
                downloaded_bytes,
                total_bytes,
            } => {
                let progress = t_args(
                    "agent-progress-downloading",
                    &[("name", model.package.name.as_str().into())],
                );
                body.push(row(Box::new(
                    ProgressBar::new(phase_progress(&model.phase).unwrap_or(0.0))
                        .with_label(progress),
                )));
                let done = byte_size(*downloaded_bytes);
                let total = byte_size(*total_bytes);
                body.push(note(&t_args(
                    "agent-download-progress",
                    &[("done", done.as_str().into()), ("total", total.into())],
                )));
            }
            LocalModelPhase::Installing { percent, message } => {
                let progress = t_args(
                    "agent-progress-installing",
                    &[("name", model.package.name.as_str().into())],
                );
                body.push(row(Box::new(match percent {
                    Some(_) => ProgressBar::new(phase_progress(&model.phase).unwrap_or(0.0))
                        .with_label(progress),
                    None => ProgressBar::indeterminate().with_label(progress),
                })));
                body.push(note(message));
            }
            LocalModelPhase::Error { message } => {
                body.push(note(message));
                body.push(note(&t("agent-error-retry-caption")));
            }
            LocalModelPhase::Ready { running: true, .. } => {
                body.push(note(&t("agent-ready-running-caption")));
            }
            LocalModelPhase::Ready { running: false, .. } => {
                body.push(note(&t("agent-ready-stopped-caption")));
            }
        }
        let actions = available_actions(&model.phase);
        if !actions.is_empty() {
            let mut buttons = Row::new();
            for action in actions {
                buttons = buttons.push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!(
                            "agent-models-{}-{}",
                            action_slug(action),
                            token(&model.package.id)
                        ),
                        &action_label(action),
                    ),
                    model_effect(model, action),
                )));
            }
            body.push(row(Box::new(buttons)));
        }
        card_with_action(
            &model.package.name,
            semantic_status_pill(&status, phase_tone(&model.phase)),
            rows(body),
        )
    }

    fn key_buffer(&self, profile_id: &str) -> Arc<Mutex<String>> {
        let mut buffers = self
            .provider_key_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        buffers
            .entry(profile_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(String::new())))
            .clone()
    }

    fn field_buffers(&self, profile: &ProviderProfile) -> HostedFieldBuffers {
        let mut buffers = self
            .provider_field_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        buffers
            .entry(profile.id.clone())
            .or_insert_with(|| HostedFieldBuffers {
                label: Arc::new(Mutex::new(profile.label.clone())),
                endpoint: Arc::new(Mutex::new(profile.endpoint.clone())),
                model: Arc::new(Mutex::new(
                    profile.default_model.clone().unwrap_or_default(),
                )),
            })
            .clone()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the hosted-provider editor keeps each credential and endpoint control together"
    )]
    fn hosted_profiles(&self) -> SettingsSection {
        let config = self.providers.get().config.clone();
        let mut body: Vec<Item> = vec![note(&t("agent-hosted-intro"))];
        body.push(row(Box::new(
            Row::new()
                .push(Box::new(self.cx.bind_invocation(
                    Button::new("agent-models-add-anthropic", &t("agent-add-anthropic")),
                    AgentModelsEffect::AddProvider {
                        protocol: "anthropic".into(),
                    },
                )))
                .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                .push(Box::new(self.cx.bind_invocation(
                    Button::new("agent-models-add-openai", &t("agent-add-compatible")),
                    AgentModelsEffect::AddProvider {
                        protocol: "openai-compatible".into(),
                    },
                ))),
        )));
        for profile in config
            .profiles
            .iter()
            .filter(|profile| profile.location != EndpointLocation::ThisDevice)
        {
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
                        .with_placeholder(&t("agent-placeholder-name")),
                ),
            ));
            body.push(field_row(
                &t("agent-placeholder-url"),
                Box::new(
                    TextInput::local()
                        .with_id(&format!("agent-models-url-{profile_id}"))
                        .with_external_buffer(Arc::clone(&fields.endpoint))
                        .with_initial_value(&endpoint)
                        .with_placeholder(&t("agent-placeholder-url")),
                ),
            ));
            body.push(field_row(
                &t("agent-placeholder-model"),
                Box::new(
                    TextInput::local()
                        .with_id(&format!("agent-models-model-{profile_id}"))
                        .with_external_buffer(Arc::clone(&fields.model))
                        .with_initial_value(&model)
                        .with_placeholder(&t("agent-placeholder-model")),
                ),
            ));
            let alternate_protocol = match profile.protocol {
                ProviderProtocol::Anthropic => ProviderProtocol::OpenAiCompatible,
                ProviderProtocol::OpenAiCompatible => ProviderProtocol::Anthropic,
            };
            let alternate_protocol_label = t_args(
                "agent-use-protocol",
                &[("protocol", alternate_protocol.label().into())],
            );
            let save_id = profile_id.clone();
            let save_fields = fields.clone();
            let read = |field: &Arc<Mutex<String>>| {
                field.lock().map(|value| value.clone()).unwrap_or_default()
            };
            body.push(row(Box::new(
                Row::new()
                    .push(Box::new(self.cx.bind(
                        Button::new(
                            &format!("agent-models-save-provider-{profile_id}"),
                            &t("agent-save-details"),
                        ),
                        effect(AgentModelsEffect::SET_PROVIDER_DETAILS).map(move |()| {
                            let model = read(&save_fields.model);
                            (
                                save_id.clone(),
                                read(&save_fields.label),
                                read(&save_fields.endpoint),
                                (!model.trim().is_empty()).then_some(model),
                            )
                        }),
                    )))
                    .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(
                            &format!("agent-models-protocol-{profile_id}"),
                            &alternate_protocol_label,
                        ),
                        AgentModelsEffect::SetProviderProtocol {
                            provider: profile_id.clone(),
                            protocol: protocol_slug(alternate_protocol).into(),
                        },
                    )))
                    .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(
                            &format!("agent-models-enabled-{profile_id}"),
                            &if profile.enabled {
                                t("agent-disable")
                            } else {
                                t("agent-enable")
                            },
                        ),
                        AgentModelsEffect::SetProviderEnabled {
                            provider: profile_id.clone(),
                            on: !profile.enabled,
                        },
                    )))
                    .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(
                            &format!("agent-models-remove-provider-{profile_id}"),
                            &t("agent-remove-provider"),
                        ),
                        AgentModelsEffect::RemoveProvider {
                            provider: profile_id.clone(),
                        },
                    ))),
            )));
            let protocol = t("agent-protocol");
            body.push(value_row(&protocol, profile.protocol.label()));
            let buffer = self.key_buffer(&profile_id);
            let initial = buffer.lock().map(|key| key.clone()).unwrap_or_default();
            let (configured, configured_error) = match self.keys.get(&profile_id) {
                Some(Ok(configured)) => (*configured, None),
                Some(Err(error)) => (false, Some(error.clone())),
                None => (false, None),
            };
            if let Some(error) = configured_error {
                body.push(row(crate::widgets::error_caption(&t_args(
                    "agent-key-read-error",
                    &[("error", error.into())],
                ))));
            }
            body.push(field_row(
                &t("agent-placeholder-key"),
                Box::new(
                    TextInput::local()
                        .with_id(&format!("agent-models-api-key-{profile_id}"))
                        .with_external_buffer(buffer)
                        .with_initial_value(&initial)
                        .with_secret(true)
                        .with_placeholder(&t("agent-placeholder-key")),
                ),
            ));
            body.push(row(Box::new(
                Row::new()
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(
                            &format!("agent-models-set-api-key-{profile_id}"),
                            &if configured {
                                t("agent-replace-key")
                            } else {
                                t("agent-save-key")
                            },
                        ),
                        AgentModelsEffect::SaveProviderKey {
                            provider: profile_id.clone(),
                            key: None,
                        },
                    )))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(
                            &format!("agent-models-remove-api-key-{profile_id}"),
                            &t("agent-remove-key"),
                        ),
                        AgentModelsEffect::RemoveProviderKey {
                            provider: profile_id.clone(),
                        },
                    ))),
            )));
            // why: a key-status sentence or badge is a status line, not a label/control pair.
            body.push(row(if configured {
                Box::new(Text::new(&t("agent-key-stored")).dimmed())
            } else if self.needs_key(profile) {
                semantic_status_pill(&t("agent-key-needed"), StatusTone::Warning)
            } else {
                Box::new(Text::new(&t("agent-key-none")).dimmed())
            }));
            if profile.default_model.is_some() {
                body.push(row(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("agent-models-default-{profile_id}"),
                        &if config.default_profile.as_deref() == Some(&profile.id) {
                            t("agent-default-current")
                        } else {
                            t("agent-use-default")
                        },
                    ),
                    AgentModelsEffect::SetDefaultProvider {
                        provider: profile_id.clone(),
                    },
                ))));
            }
        }
        for issue in config.validation_errors() {
            body.push(note(&t_args(
                "agent-config-issue",
                &[("message", issue.message.as_str().into())],
            )));
        }
        card(&t("agent-hosted-title"), rows(body))
    }

    fn request_limit_buffers(
        &self,
        profile: &ProviderProfile,
        selected: &ModelDescriptor,
    ) -> LimitFieldBuffers {
        let key = (profile.id.clone(), selected.id.clone());
        let mut buffers = self
            .limit_field_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    fn request_limits_card(&self) -> SettingsSection {
        let config = self.providers.get().config.clone();
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
                                .with_placeholder(&t("agent-context-unknown")),
                        ),
                    ));
                }
                let provider = profile.id.clone();
                let model = selected.id.clone();
                body.push(row(Box::new(
                    Row::new()
                        .push(Box::new(self.cx.bind(
                            Button::new(
                                &format!("agent-model-limit-save-{widget_id}"),
                                &t("agent-save-details"),
                            ),
                            effect(AgentModelsEffect::SET_MODEL_REQUEST_LIMITS).map(move |()| {
                                (
                                    provider.clone(),
                                    model.clone(),
                                    limit_input(&fields.output),
                                    limit_input(&fields.runtime),
                                    limit_input(&fields.deadline),
                                )
                            }),
                        )))
                        .push(Box::new(self.cx.bind_invocation(
                            Button::new(
                                &format!("agent-model-limit-clear-{widget_id}"),
                                &t("agent-request-limits-clear"),
                            ),
                            AgentModelsEffect::SetModelRequestLimits {
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
        card(&t("agent-request-limits-title"), rows(body))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let private_title = t("agent-private-title");
        let local_inf = t("agent-local-inference");
        let local_gw = t("agent-local-gateway");
        let downloads = t("agent-downloads");
        let downloads_v = t("agent-downloads-value");
        let mut cards = vec![card(
            &private_title,
            rows(vec![
                note(&t("agent-private-caption")),
                value_row(&local_inf, &local_gw),
                value_row(&downloads, &downloads_v),
            ]),
        )];
        if !self.manager_available {
            cards.push(card(
                &t("agent-service-down-title"),
                rows(vec![note(&t("agent-service-down-body"))]),
            ));
            cards.push(self.hosted_profiles());
            cards.push(self.request_limits_card());
            return crate::widgets::cards(cards);
        }
        if let Some(notice) = &self.notice {
            cards.push(card(&t("agent-service-title"), rows(vec![note(notice)])));
        }
        if let Some(accelerator) = self.accelerator_card() {
            cards.push(accelerator);
        }
        // A link to one in-process model puts its card first, where the
        // person who followed it sees it without hunting.
        let revealed = self
            .revealed
            .as_deref()
            .and_then(|id| self.snapshot.in_process_model(id));
        if let Some(model) = revealed {
            cards.push(self.in_process_card(model));
        }
        cards.push(self.recommendation_card());
        for model in &self.snapshot.in_process {
            if revealed.is_none_or(|shown| shown.package.id != model.package.id) {
                cards.push(self.in_process_card(model));
            }
        }
        if self.snapshot.models.is_empty() {
            cards.push(card(
                &t("agent-models-empty-title"),
                rows(vec![note(&t("agent-models-empty"))]),
            ));
        } else {
            for model in &self.snapshot.models {
                cards.push(self.model_card(model));
            }
        }
        cards.push(self.hosted_profiles());
        cards.push(self.request_limits_card());
        crate::widgets::cards(cards)
    }
}

/// Change only the selected model's policy, retaining other profile metadata.
fn set_model_limits(
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

fn gib(n: impl Into<i64>) -> String {
    t_args("agent-gib", &[("n", n.into().into())])
}

fn fit_tone(fit: HardwareFit) -> StatusTone {
    match fit {
        HardwareFit::Excellent | HardwareFit::Good => StatusTone::Success,
        HardwareFit::Tight | HardwareFit::Unsupported => StatusTone::Warning,
    }
}

fn phase_tone(phase: &LocalModelPhase) -> StatusTone {
    match phase {
        LocalModelPhase::Ready { running: true, .. } => StatusTone::Success,
        LocalModelPhase::Ready { running: false, .. } => StatusTone::Neutral,
        LocalModelPhase::Downloading { .. } | LocalModelPhase::Installing { .. } => {
            StatusTone::Info
        }
        LocalModelPhase::Error { .. } => StatusTone::Warning,
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(750);
const MANAGER_OUTPUT_LIMIT: usize = 1024 * 1024;

fn manager_run(args: &[&str], timeout: Duration) -> Result<bounded_command::Output, String> {
    bounded_command::run(
        "tilo-model-manager",
        args,
        None,
        MANAGER_OUTPUT_LIMIT,
        timeout,
    )
    .map_err(|error| {
        t_args(
            "agent-manager-unavailable",
            &[("error", error.to_string().into())],
        )
    })
}

#[derive(Clone)]
struct HostedFieldBuffers {
    label: Arc<Mutex<String>>,
    endpoint: Arc<Mutex<String>>,
    model: Arc<Mutex<String>>,
}

#[derive(Clone)]
struct LimitFieldBuffers {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareFit {
    Excellent,
    Good,
    Tight,
    Unsupported,
}

impl HardwareFit {
    fn label(self) -> String {
        t(match self {
            Self::Excellent => "agent-fit-excellent",
            Self::Good => "agent-fit-good",
            Self::Tight => "agent-fit-tight",
            Self::Unsupported => "agent-fit-unsupported",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareRecommendation {
    pub fit: HardwareFit,
    pub memory_gib: u16,
    pub accelerator: String,
    pub accelerator_memory_gib: Option<u16>,
    pub free_storage_gib: Option<u16>,
    pub reason: String,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag mirrors an independent signed declaration of the release"
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelPackage {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub license_url: String,
    pub download_bytes: u64,
    pub installed_bytes: u64,
    /// Signed capability reported by the local-model manager for this exact
    /// revision. It is not inferred from a model name.
    #[serde(default)]
    pub vision: bool,
    /// Signed biometric capability for a distinct local face runtime.  A
    /// general image-analysis model must not gain this capability implicitly.
    #[serde(default)]
    pub face_recognition: bool,
    /// The model runs inside the apps that use it: it is installed, never
    /// started, and is not a chat model for the local provider profile.
    #[serde(default)]
    pub in_process: bool,
    /// Signed salient-object segmentation capability ("Select subject").
    #[serde(default)]
    pub segmentation: bool,
}

impl LocalModelPackage {
    /// The card title naming what an in-process model does for the person.
    fn capability_title(&self) -> String {
        if self.segmentation {
            t("agent-inprocess-segmentation-title")
        } else {
            self.name.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LocalModelPhase {
    Downloading {
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    Installing {
        percent: Option<u8>,
        message: String,
    },
    Ready {
        running: bool,
        /// Exact loopback endpoint published by the supervised model service.
        /// A missing endpoint is never substituted with a guessed port.
        #[serde(default)]
        endpoint: Option<LocalModelEndpoint>,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelEndpoint {
    pub url: String,
    pub model_id: String,
}

impl LocalModelPhase {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Downloading { .. } | Self::Installing { .. })
    }

    fn status(&self) -> String {
        t(match self {
            Self::Downloading { .. } => "agent-status-downloading",
            Self::Installing { .. } => "agent-status-installing",
            Self::Ready { running: true, .. } => "agent-status-running",
            Self::Ready { running: false, .. } => "agent-status-ready",
            Self::Error { .. } => "agent-status-attention",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledLocalModel {
    pub package: LocalModelPackage,
    pub phase: LocalModelPhase,
}

/// A signed model that runs in-process, offered whether or not it is
/// installed. `phase` is absent until it is set up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InProcessLocalModel {
    pub package: LocalModelPackage,
    #[serde(default)]
    pub phase: Option<LocalModelPhase>,
}

impl InProcessLocalModel {
    /// Whether setting it up (again) is what the card offers.
    pub(crate) fn installable(&self) -> bool {
        matches!(self.phase, None | Some(LocalModelPhase::Error { .. }))
    }

    /// Whether there are local bytes or a failed record to remove.
    pub(crate) fn removable(&self) -> bool {
        matches!(
            self.phase,
            Some(LocalModelPhase::Ready { .. } | LocalModelPhase::Error { .. })
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AcceleratorSetupState {
    Available,
    Queued,
    Staging,
    RestartRequired,
    Active,
    Error { message: String },
}

impl AcceleratorSetupState {
    pub(crate) fn blocks_model_setup(&self) -> bool {
        !matches!(self, Self::Active)
    }

    fn is_transient(&self) -> bool {
        matches!(self, Self::Queued | Self::Staging)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceleratorSetup {
    pub boot_pack_id: String,
    pub status: AcceleratorSetupState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelSnapshot {
    pub hardware: HardwareRecommendation,
    pub recommendation: Option<LocalModelPackage>,
    #[serde(default)]
    pub models: Vec<InstalledLocalModel>,
    /// Every signed in-process model, installed or not.
    #[serde(default)]
    pub in_process: Vec<InProcessLocalModel>,
    #[serde(default)]
    pub accelerator_setup: Option<AcceleratorSetup>,
}

impl LocalModelSnapshot {
    pub(crate) fn in_process_model(&self, id: &str) -> Option<&InProcessLocalModel> {
        self.in_process.iter().find(|model| model.package.id == id)
    }
}

/// Boundary implemented by Tilo' model service. The production adapter is a
/// deliberately tiny JSON CLI seam so Settings does not acquire downloader,
/// container-runtime, GPU-driver, or privilege-management dependencies.
#[allow(
    clippy::missing_errors_doc,
    reason = "the six adapter operations share one error contract documented on the trait"
)]
pub trait LocalModelControl: Send + Sync {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String>;
    fn install(&self, model: &LocalModelPackage) -> Result<(), String>;
    fn start(&self, id: &str) -> Result<(), String>;
    fn stop(&self, id: &str) -> Result<(), String>;
    fn remove(&self, id: &str) -> Result<(), String>;
    fn stage_accelerator(&self) -> Result<(), String>;
}

pub struct ModelManagerCli;

impl ModelManagerCli {
    fn command(verb: &str, id: &str) -> Result<(), String> {
        let output = manager_run(&[verb, "--model", id], Duration::from_secs(10 * 60))?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| format!("model service refused {verb}"))
    }
}

impl LocalModelControl for ModelManagerCli {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        let output = manager_run(&["snapshot", "--format", "json"], Duration::from_secs(10))?;
        if !output.status.success() {
            return Err(output.stderr_diagnostic());
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("model service returned invalid status: {error}"))
    }

    fn install(&self, model: &LocalModelPackage) -> Result<(), String> {
        let output = manager_run(
            &[
                "install",
                "--model",
                &model.id,
                "--revision",
                &model.revision,
                "--accept-license",
            ],
            Duration::from_secs(30 * 60),
        )?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| "model service refused the installation".to_string())
    }

    fn start(&self, id: &str) -> Result<(), String> {
        Self::command("start", id)
    }
    fn stop(&self, id: &str) -> Result<(), String> {
        Self::command("stop", id)
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        Self::command("remove", id)
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        let output = manager_run(&["stage-accelerator"], Duration::from_secs(30 * 60))?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| "accelerator setup service refused the request".to_string())
    }
}

fn fallback_snapshot() -> LocalModelSnapshot {
    let memory_gib = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("MemTotal:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
        })
        .and_then(|kib| u16::try_from(kib / 1024 / 1024).ok())
        .unwrap_or(8);
    let (fit, reason) = if memory_gib >= 32 {
        (HardwareFit::Excellent, t("agent-reason-excellent"))
    } else if memory_gib >= 16 {
        (HardwareFit::Good, t("agent-reason-good"))
    } else if memory_gib >= 8 {
        (HardwareFit::Tight, t("agent-reason-tight"))
    } else {
        (HardwareFit::Unsupported, t("agent-reason-unsupported"))
    };
    LocalModelSnapshot {
        hardware: HardwareRecommendation {
            fit,
            memory_gib,
            accelerator: t("agent-accel-checked"),
            accelerator_memory_gib: None,
            free_storage_gib: None,
            reason,
        },
        recommendation: None,
        models: Vec::new(),
        in_process: Vec::new(),
        accelerator_setup: None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelAction {
    Start,
    Stop,
    Remove,
    Retry,
}

fn available_actions(phase: &LocalModelPhase) -> Vec<ModelAction> {
    match phase {
        LocalModelPhase::Ready { running: true, .. } => {
            vec![ModelAction::Stop, ModelAction::Remove]
        }
        LocalModelPhase::Ready { running: false, .. } => {
            vec![ModelAction::Start, ModelAction::Remove]
        }
        LocalModelPhase::Error { .. } => vec![ModelAction::Retry, ModelAction::Remove],
        LocalModelPhase::Downloading { .. } | LocalModelPhase::Installing { .. } => Vec::new(),
    }
}

fn action_label(action: ModelAction) -> String {
    t(match action {
        ModelAction::Start => "agent-action-start",
        ModelAction::Stop => "agent-action-stop",
        ModelAction::Remove => "agent-action-remove",
        ModelAction::Retry => "agent-action-retry",
    })
}

fn action_slug(action: ModelAction) -> &'static str {
    match action {
        ModelAction::Start => "start",
        ModelAction::Stop => "stop",
        ModelAction::Remove => "remove",
        ModelAction::Retry => "retry",
    }
}

fn byte_size(bytes: u64) -> String {
    // In-process models are a few hundred megabytes; "0.2 GiB" says little.
    if bytes < 1_073_741_824 {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{:.1} GiB", bytes as f64 / 1_073_741_824.0)
    }
}

fn phase_progress(phase: &LocalModelPhase) -> Option<f32> {
    match phase {
        LocalModelPhase::Downloading {
            downloaded_bytes,
            total_bytes,
        } if *total_bytes > 0 => Some(*downloaded_bytes as f32 / *total_bytes as f32),
        LocalModelPhase::Installing {
            percent: Some(percent),
            ..
        } => Some(f32::from(*percent) / 100.0),
        _ => None,
    }
}

fn synced_local_profile(
    existing: Option<ProviderProfile>,
    models: &[InstalledLocalModel],
) -> Option<ProviderProfile> {
    let ready = models
        .iter()
        .filter(|model| {
            !model.package.in_process && matches!(model.phase, LocalModelPhase::Ready { .. })
        })
        .collect::<Vec<_>>();
    if ready.is_empty() {
        return None;
    }
    let running_endpoint = ready.iter().find_map(|model| match &model.phase {
        LocalModelPhase::Ready {
            running: true,
            endpoint: Some(endpoint),
        } if endpoint.model_id == model.package.id
            && (endpoint.url.starts_with("http://127.0.0.1:")
                || endpoint.url.starts_with("http://[::1]:")) =>
        {
            Some(endpoint)
        }
        _ => None,
    });
    // Do not manufacture a disabled profile holding `:8000` merely because
    // bytes are installed. The dynamic endpoint exists only when the manager
    // has actually started and validated the loopback server.
    if existing.is_none() && running_endpoint.is_none() {
        return None;
    }
    let mut profile = existing.unwrap_or_else(ProviderProfile::local_vllm);
    profile.models = ready
        .iter()
        .map(|model| ModelDescriptor {
            id: model.package.id.clone(),
            label: model.package.name.clone(),
            context_window: profile
                .models
                .iter()
                .find(|old| old.id == model.package.id)
                .and_then(|old| old.context_window),
            request_limits: profile
                .models
                .iter()
                .find(|old| old.id == model.package.id)
                .and_then(|old| old.request_limits.clone()),
            revision: Some(model.package.revision.clone()),
            vision: model.package.vision,
            face_recognition: model.package.face_recognition,
            tool_calling: true,
        })
        .collect();
    profile.default_model = profile
        .default_model
        .filter(|id| profile.models.iter().any(|model| model.id == *id))
        .or_else(|| profile.models.first().map(|model| model.id.clone()));
    // The manager owns port allocation. Settings persists only the endpoint
    // returned by its ready state, never the legacy `:8000` placeholder.
    if let Some(endpoint) = running_endpoint {
        profile.endpoint.clone_from(&endpoint.url);
        profile.default_model = Some(endpoint.model_id.clone());
    }
    profile.enabled = running_endpoint.is_some();
    Some(profile)
}

impl PaneModel for AgentModelsModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        if self.providers.refresh() {
            self.read_keys();
        }
        if self.is_busy() && self.last_poll.elapsed() >= POLL_INTERVAL {
            self.refresh();
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "AgentModelsPane"
    }

    fn busy(&self) -> bool {
        self.is_busy()
    }
}

impl Drop for AgentModelsModel {
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

/// The model manager a screenshot and the crate's tests show: a good fit,
/// one recommended model, nothing installed. Every request succeeds.
struct SampleManager;

impl LocalModelControl for SampleManager {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        Ok(LocalModelSnapshot {
            hardware: HardwareRecommendation {
                fit: HardwareFit::Good,
                memory_gib: 32,
                accelerator: "GPU with 16 GiB".into(),
                accelerator_memory_gib: Some(16),
                free_storage_gib: Some(120),
                reason: t("agent-reason-good"),
            },
            recommendation: Some(LocalModelPackage {
                id: "sample/helpful-7b".into(),
                name: "Helpful 7B".into(),
                description: "A capable assistant model for this device".into(),
                source: "huggingface.co/sample/helpful-7b".into(),
                revision: "0123456789abcdef".into(),
                license: "Apache 2.0".into(),
                license_url: "https://www.apache.org/licenses/LICENSE-2.0".into(),
                download_bytes: 5_368_709_120,
                installed_bytes: 10_737_418_240,
                vision: false,
                face_recognition: false,
                in_process: false,
                segmentation: false,
            }),
            models: Vec::new(),
            in_process: Vec::new(),
            accelerator_setup: None,
        })
    }
    fn install(&self, _: &LocalModelPackage) -> Result<(), String> {
        Ok(())
    }
    fn start(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn stop(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use super::*;

    fn package() -> LocalModelPackage {
        LocalModelPackage {
            id: "test/model".into(),
            name: "Helpful 7B".into(),
            description: "Test package".into(),
            source: "huggingface.co/test/model".into(),
            revision: "0123456789abcdef".into(),
            license: "Apache 2.0".into(),
            license_url: "https://example.test/license".into(),
            download_bytes: 5_368_709_120,
            installed_bytes: 10_737_418_240,
            vision: false,
            face_recognition: false,
            in_process: false,
            segmentation: false,
        }
    }

    fn subject_package() -> LocalModelPackage {
        LocalModelPackage {
            id: "u2net-subject".into(),
            name: "Subject selection (U²-Net)".into(),
            description: "Finds the main subject of a photo".into(),
            source: "https://artifacts.example.test/u2net.onnx".into(),
            revision: "8d10f".into(),
            license: "Apache-2.0".into(),
            license_url: "https://artifacts.example.test/LICENSE".into(),
            download_bytes: 175_997_641,
            installed_bytes: 175_997_641,
            vision: false,
            face_recognition: false,
            in_process: true,
            segmentation: true,
        }
    }

    fn with_subject(phase: Option<LocalModelPhase>) -> LocalModelSnapshot {
        LocalModelSnapshot {
            in_process: vec![InProcessLocalModel {
                package: subject_package(),
                phase,
            }],
            ..snapshot(None)
        }
    }

    fn rendered_names(model: &AgentModelsModel) -> Vec<String> {
        let composition = model.build_composition();
        let mut nodes = Vec::new();
        collect_accessibility(composition.as_ref(), &mut nodes);
        nodes.into_iter().filter_map(|node| node.name).collect()
    }

    /// An in-process model has its own card with the recommendation's
    /// licence confirmation, a link opens it at that confirmation without
    /// downloading anything, and the install action is itself the
    /// acceptance of the revision it was offered with.
    #[test]
    fn in_process_models_are_set_up_with_the_same_confirmation() {
        // Case `setup_confirms_before_install`.
        {
            let control = mock(with_subject(None));
            let model = model_over(control.clone());
            // The recommendation's and the subject model's own.
            assert_eq!(
                rendered_names(&model)
                    .iter()
                    .filter(|name| *name == "Set up on this device")
                    .count(),
                2
            );
            model.reviewing_local.set(Some("u2net-subject".into()));
            assert!(
                rendered_names(&model)
                    .iter()
                    .any(|name| name == "Accept license & download")
            );
            assert!(control.calls.lock().unwrap().is_empty());
            model
                .manager_work(&AgentModelsEffect::InstallModel {
                    model: "u2net-subject".into(),
                    revision: "8d10f".into(),
                })
                .unwrap()
                .run(control.as_ref())
                .unwrap();
            assert_eq!(
                control.calls.lock().unwrap().as_slice(),
                ["install:u2net-subject@8d10f"]
            );
            assert!(
                model
                    .manager_work(&AgentModelsEffect::InstallModel {
                        model: "u2net-subject".into(),
                        revision: "stale".into(),
                    })
                    .is_err(),
                "the acceptance holds only for the revision it was offered with"
            );
        }

        // Case `a_link_reveals_the_card_at_its_confirmation`.
        {
            let control = mock(with_subject(None));
            let mut model = model_over(control.clone());
            model.reveal("u2net-subject").unwrap();
            assert_eq!(
                model.reviewing_local.with(Clone::clone).as_deref(),
                Some("u2net-subject")
            );
            assert_eq!(model.revealed.as_deref(), Some("u2net-subject"));
            assert!(control.calls.lock().unwrap().is_empty());
            assert!(model.reveal("no-such-model").is_err());
        }

        // Case `each_model_is_a_row_with_its_install`.
        {
            let model = model_over(mock(with_subject(None)));
            let area = AgentModelsArea {
                cx: model.cx.clone(),
                model: std::cell::OnceCell::from(Rc::new(std::cell::RefCell::new(model))),
                read: crate::device_read::DeviceRead::default(),
            };
            let published = area
                .rows()
                .into_iter()
                .find(|row| row.id() == "settings.agent-model:u2net-subject")
                .expect("the in-process model is a row")
                .into_resource();
            assert!(
                published
                    .actions
                    .iter()
                    .any(|action| action.id == "install-model")
            );
            assert!(
                !published
                    .actions
                    .iter()
                    .any(|action| action.id == "remove-model")
            );
        }
    }

    /// A ready in-process model offers only Remove, a destructive action the
    /// host confirms, and never joins the local chat profile.
    #[test]
    fn ready_in_process_models_offer_only_a_confirmed_remove() {
        let ready = LocalModelPhase::Ready {
            running: false,
            endpoint: None,
        };
        let control = mock(with_subject(Some(ready.clone())));
        let model = model_over(control.clone());
        let names = rendered_names(&model);
        assert!(!names.iter().any(|name| name == "Start"));
        assert_eq!(
            names
                .iter()
                .filter(|name| *name == "Set up on this device")
                .count(),
            1,
            "only the recommendation still offers setup"
        );
        let remove = SettingsEffect::from(AgentModelsEffect::RemoveModel {
            model: "u2net-subject".into(),
        })
        .bound();
        assert_eq!(
            remove.contract.confirmation,
            ui::tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            model
                .manager_work(&AgentModelsEffect::StartModel {
                    model: "u2net-subject".into(),
                })
                .is_err(),
            "an in-process model is never started"
        );
        model
            .manager_work(&AgentModelsEffect::RemoveModel {
                model: "u2net-subject".into(),
            })
            .unwrap()
            .run(control.as_ref())
            .unwrap();
        assert_eq!(
            control.calls.lock().unwrap().as_slice(),
            ["remove:u2net-subject"]
        );

        let installed = vec![InstalledLocalModel {
            package: subject_package(),
            phase: ready,
        }];
        assert!(synced_local_profile(None, &installed).is_none());
    }

    fn snapshot(phase: Option<LocalModelPhase>) -> LocalModelSnapshot {
        let package = package();
        LocalModelSnapshot {
            hardware: HardwareRecommendation {
                fit: HardwareFit::Good,
                memory_gib: 24,
                accelerator: "GPU with 12 GiB".into(),
                accelerator_memory_gib: Some(12),
                free_storage_gib: Some(80),
                reason: "Balanced for this device".into(),
            },
            recommendation: Some(package.clone()),
            models: phase
                .map(|phase| vec![InstalledLocalModel { package, phase }])
                .unwrap_or_default(),
            in_process: Vec::new(),
            accelerator_setup: None,
        }
    }

    struct MockControl {
        snapshot: Mutex<LocalModelSnapshot>,
        calls: Mutex<Vec<String>>,
    }

    impl LocalModelControl for MockControl {
        fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
            Ok(self.snapshot.lock().unwrap().clone())
        }
        fn install(&self, model: &LocalModelPackage) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("install:{}@{}", model.id, model.revision));
            Ok(())
        }
        fn start(&self, id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("start:{id}"));
            Ok(())
        }
        fn stop(&self, id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("stop:{id}"));
            Ok(())
        }
        fn remove(&self, id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("remove:{id}"));
            Ok(())
        }
        fn stage_accelerator(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("stage-accelerator".into());
            Ok(())
        }
    }

    fn collect_accessibility(widget: &dyn Widget, nodes: &mut Vec<ui::a11y::AccessNode>) {
        if let Some(node) = widget.accessibility() {
            nodes.push(node);
        }
        for child in widget.children() {
            collect_accessibility(child, nodes);
        }
    }

    /// A hosted provider whose key this device's vault lacks says so and
    /// offers to take one; once the vault holds it, it says it is stored.
    #[test]
    fn a_hosted_provider_without_a_key_here_asks_for_one() {
        let mut pane = model_over(mock(snapshot(None)));
        pane.change_providers(AgentModelsEffect::AddProvider {
            protocol: "anthropic".into(),
        })
        .unwrap();
        let profile = pane.providers.get().config.profiles[0].clone();
        pane.keys.insert(profile.id.clone(), Ok(false));
        assert!(pane.needs_key(&profile));
        let text = crate::widgets::announced_text(pane.compose().as_ref());
        assert!(
            text.iter().any(|line| line == "Needs a key on this device"),
            "{text:#?}"
        );

        pane.keys.insert(profile.id.clone(), Ok(true));
        assert!(!pane.needs_key(&profile));
        let save = SettingsEffect::from(AgentModelsEffect::SaveProviderKey {
            provider: profile.id,
            key: None,
        })
        .bound_asking(&["key"]);
        assert!(save.params().iter().any(|param| param.name == "key"));
    }

    fn model_over(control: Arc<MockControl>) -> AgentModelsModel {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let mut model = AgentModelsModel::new(cx, control);
        model.follow_local_profile = false;
        model
    }

    fn mock(snapshot: LocalModelSnapshot) -> Arc<MockControl> {
        Arc::new(MockControl {
            snapshot: Mutex::new(snapshot),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// No fabricated recommendations; installs are of the reported
    /// package, and progress is bounded and readable.
    #[test]
    fn the_catalog_is_honest_and_installs_what_it_reported() {
        let fallback = fallback_snapshot();
        assert!(fallback.recommendation.is_none());
        let pane = model_over(mock(fallback));
        let composition = pane.build_composition();
        let mut nodes = Vec::new();
        collect_accessibility(composition.as_ref(), &mut nodes);
        let setup = nodes
            .iter()
            .find(|node| node.name.as_deref() == Some("Set up unavailable"))
            .expect("disabled setup remains understandable to assistive technology");
        assert!(setup.state.disabled);

        let control = mock(snapshot(None));
        let pane = model_over(control.clone());
        let work = pane
            .manager_work(&AgentModelsEffect::InstallModel {
                model: "test/model".into(),
                revision: "0123456789abcdef".into(),
            })
            .unwrap();
        work.run(control.as_ref()).unwrap();
        assert_eq!(
            control.calls.lock().unwrap().as_slice(),
            ["install:test/model@0123456789abcdef"]
        );
        assert!(
            pane.manager_work(&AgentModelsEffect::InstallModel {
                model: "test/model".into(),
                revision: "stale".into(),
            })
            .is_err(),
            "only the reported revision is installed"
        );

        let phase = LocalModelPhase::Downloading {
            downloaded_bytes: 3,
            total_bytes: 4,
        };
        assert_eq!(phase_progress(&phase), Some(0.75));
        let progress = ProgressBar::new(phase_progress(&phase).unwrap()).with_label("Download");
        let access = Widget::accessibility(&progress).expect("labelled progress is accessible");
        assert_eq!(access.value.as_deref(), Some("75%"));
    }

    /// Only the controls a model's phase allows reach the manager.
    #[test]
    fn local_model_lifecycle_exposes_only_safe_controls() {
        assert_eq!(
            available_actions(&LocalModelPhase::Ready {
                running: false,
                endpoint: None
            }),
            [ModelAction::Start, ModelAction::Remove]
        );
        assert_eq!(
            available_actions(&LocalModelPhase::Ready {
                running: true,
                endpoint: None
            }),
            [ModelAction::Stop, ModelAction::Remove]
        );
        assert!(
            available_actions(&LocalModelPhase::Downloading {
                downloaded_bytes: 4,
                total_bytes: 10,
            })
            .is_empty()
        );
        let control = mock(LocalModelSnapshot {
            accelerator_setup: Some(AcceleratorSetup {
                boot_pack_id: "gpu-stack".into(),
                status: AcceleratorSetupState::Available,
            }),
            ..snapshot(Some(LocalModelPhase::Ready {
                running: false,
                endpoint: None,
            }))
        });
        let pane = model_over(control.clone());
        for effect in [
            AgentModelsEffect::StartModel {
                model: "test/model".into(),
            },
            AgentModelsEffect::RemoveModel {
                model: "test/model".into(),
            },
            AgentModelsEffect::SetUpAccelerator,
        ] {
            pane.manager_work(&effect)
                .unwrap()
                .run(control.as_ref())
                .unwrap();
        }
        assert!(
            pane.manager_work(&AgentModelsEffect::StopModel {
                model: "test/model".into()
            })
            .is_err(),
            "a stopped model is not stopped again"
        );
        assert_eq!(
            control.calls.lock().unwrap().as_slice(),
            ["start:test/model", "remove:test/model", "stage-accelerator"]
        );
    }

    /// The local profile follows the manager's ready endpoint, not a fixed
    /// port.
    #[test]
    fn local_profile_uses_the_ready_manager_endpoint_not_port_8000() {
        let mut package = package();
        package.vision = true;
        package.face_recognition = true;
        let model_id = package.id.clone();
        let models = vec![InstalledLocalModel {
            package,
            phase: LocalModelPhase::Ready {
                running: true,
                endpoint: Some(LocalModelEndpoint {
                    url: "http://127.0.0.1:43127/v1".into(),
                    model_id,
                }),
            },
        }];
        let profile = synced_local_profile(None, &models).expect("ready local profile");
        assert_eq!(profile.endpoint, "http://127.0.0.1:43127/v1");
        assert_eq!(profile.default_model.as_deref(), Some("test/model"));
        assert_eq!(
            profile.models[0].revision.as_deref(),
            Some("0123456789abcdef")
        );
        assert!(profile.models[0].vision);
        assert!(profile.models[0].face_recognition);
        assert!(profile.enabled);
        let mut configured = profile;
        configured.models[0].context_window = Some(262_144);
        configured.models[0].request_limits = Some(ModelRequestLimits {
            max_output_tokens: 512,
            runtime_context_tokens: Some(4096),
            plan_timeout_ms: Some(120_000),
        });
        let refreshed = synced_local_profile(Some(configured.clone()), &models).unwrap();
        assert_eq!(
            refreshed.models[0].request_limits,
            configured.models[0].request_limits
        );
        assert_eq!(refreshed.models[0].context_window, Some(262_144));
    }

    #[test]
    fn model_request_limits_use_the_published_settings_effects() {
        use ui::framework::testing::TestApp;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
        profile.models = vec![
            ModelDescriptor::new("test-model"),
            ModelDescriptor::new("test_model"),
        ];
        profile.models[0].context_window = Some(262_144);
        let profile_id = profile.id.clone();
        ProviderConfig {
            profiles: vec![profile],
            default_profile: None,
        }
        .save_at(&path)
        .unwrap();
        let mut app = TestApp::<crate::resource::SettingsShell>::new().unwrap();
        app.surface_mut::<SettingsApp>()
            .attach_host(PaneCx::in_file(path.clone()), None);
        app.surface_mut::<SettingsApp>()
            .agent_models()
            .model()
            .borrow_mut()
            .follow_local_profile = false;
        let open = crate::effects::open_pane_address(crate::categories::Category::AgentModels);
        app.call_on(&open.resource, &open.action, serde_json::json!({}))
            .unwrap();
        app.settle();
        let resource = format!(
            "settings.agent-provider-model:{}:{}",
            hex::encode(&profile_id),
            hex::encode("test-model")
        );
        app.call_on(
            &resource,
            "set-model-request-limits",
            serde_json::json!({"max_output_tokens":512,"runtime_context_tokens":4096,"plan_timeout_ms":120_000}),
        )
        .unwrap();
        app.settle();
        let value = app
            .call_on(
                &resource,
                "inspect-model-request-limits",
                serde_json::json!({}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(value["request_limits"]["max_output_tokens"], 512);
        assert_eq!(value["request_limits"]["plan_timeout_ms"], 120_000);
        assert_eq!(value["plan_timeout_ceiling_ms"], 120_000);
        assert_eq!(value["caller_may_request_shorter_deadline"], true);
        assert_eq!(value["source_context_tokens"], 262_144);
        assert_eq!(value["runtime_context_attested"], false);
        assert_eq!(value["prompt_fit_proven"], false);
        let other = format!(
            "settings.agent-provider-model:{}:{}",
            hex::encode(&profile_id),
            hex::encode("test_model")
        );
        let other_value = app
            .call_on(
                &other,
                "inspect-model-request-limits",
                serde_json::json!({}),
            )
            .unwrap()
            .unwrap();
        assert!(other_value["request_limits"].is_null());
        assert_eq!(other_value["plan_timeout_ceiling_ms"], 60_000);
        let saved = fs::read(&path).unwrap();
        for invalid in [
            serde_json::json!({"max_output_tokens":4096,"runtime_context_tokens":4096}),
            serde_json::json!({"max_output_tokens":512,"plan_timeout_ms":0}),
            serde_json::json!({"max_output_tokens":512,"plan_timeout_ms":900_001}),
            serde_json::json!({"plan_timeout_ms":120_000}),
            serde_json::json!({"model":"absent-model","max_output_tokens":512}),
        ] {
            assert!(
                app.call_on(&resource, "set-model-request-limits", invalid)
                    .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), saved);
        }
        app.call_on(&resource, "set-model-request-limits", serde_json::json!({}))
            .unwrap();
        assert!(
            ProviderConfig::load_at(&path).profiles[0].models[0]
                .request_limits
                .is_none()
        );
        assert!(app.harness().component_contract_violations().is_empty());
    }

    /// Providers are the person's settings: added, renamed, made default
    /// and removed through effects, which refuse a provider that is gone.
    #[test]
    fn providers_are_the_persons_settings() {
        let mut pane = model_over(mock(snapshot(None)));
        pane.change_providers(AgentModelsEffect::AddProvider {
            protocol: "anthropic".into(),
        })
        .unwrap();
        let id = pane.providers.get().config.profiles[0].id.clone();
        pane.change_providers(AgentModelsEffect::SetProviderDetails {
            provider: id.clone(),
            label: "Work".into(),
            endpoint: "https://api.example.test".into(),
            model: Some("big-model".into()),
        })
        .unwrap();
        pane.change_providers(AgentModelsEffect::SetDefaultProvider {
            provider: id.clone(),
        })
        .unwrap();
        let stored = pane.cx.settings::<AgentProviders>().get().config;
        assert_eq!(stored.default_profile.as_deref(), Some(id.as_str()));
        assert_eq!(stored.profiles[0].label, "Work");
        assert!(
            pane.change_providers(AgentModelsEffect::SetProviderEnabled {
                provider: "gone".into(),
                on: true,
            })
            .is_err()
        );
    }

    /// Wait for a background read to end, as the warm-up's wake would.
    fn until_finished(finished: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !finished() {
            assert!(std::time::Instant::now() < deadline, "the read ends");
            std::thread::yield_now();
        }
    }

    /// The warm-up asks the manager off the UI thread once; the model is
    /// built from that answer, and nothing is asked again once it is.
    #[test]
    fn the_area_builds_its_model_from_the_read_it_started() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = AgentModelsArea::new(&cx);
        assert!(area.start_read(), "the first read starts");
        assert!(!area.start_read(), "one read at a time");
        assert!(!area.is_open(), "a running read publishes nothing yet");
        until_finished(|| area.read_finished());
        let installed = area.model().borrow().snapshot.recommendation.clone();
        assert!(area.is_open());
        assert_eq!(
            installed.map(|package| package.id),
            SampleManager
                .snapshot()
                .unwrap()
                .recommendation
                .map(|package| package.id),
            "the model shows what the read asked"
        );
        assert!(!area.start_read(), "a built model is not read again");
    }

    /// What the read asked is what the model shows: the manager's answer,
    /// or why it could not answer.
    #[test]
    fn a_model_shows_what_its_read_asked() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let control = mock(snapshot(None));
        let read = AgentModelsRead::of(control.as_ref(), Vec::new());
        let model = AgentModelsModel::from_read(cx.clone(), control.clone(), read);
        assert!(model.manager_available);
        assert_eq!(
            model
                .snapshot
                .recommendation
                .as_ref()
                .map(|package| package.id.clone()),
            Some(package().id)
        );
        let down = AgentModelsRead {
            snapshot: Err("the manager is not running".into()),
            keys: HashMap::new(),
        };
        let model = AgentModelsModel::from_read(cx, control, down);
        assert!(!model.manager_available);
        assert_eq!(
            model.notice,
            Some(t_args(
                "agent-notice-service-down",
                &[("error", "the manager is not running".into())]
            )),
            "the person is told the manager is down"
        );
    }
}
