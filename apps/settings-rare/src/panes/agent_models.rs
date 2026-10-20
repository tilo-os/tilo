//! Agent Models — device-local model lifecycle and provider profiles.
//!
//! Tilo owns the user-facing lifecycle, while a small adapter speaks to the
//! privileged model manager that pins Hugging Face snapshots and supervises
//! vLLM. Settings never downloads model bytes or starts a server itself.
//! With host administration, profiles and credentials come from the backend.
//! Without it, legacy metadata remains editable offline, never as a claim of
//! backend synchronization. Keys stay in the backend vault. Every change is an
//! [`AgentModelsEffect`].

use agent_provider_config::management::CredentialStatus;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;

pub use agent_provider_config::local::snapshot::{
    AcceleratorSetup, AcceleratorSetupState, HardwareFit, HardwareRecommendation,
    InProcessLocalModel, InstalledLocalModel, LocalModelPackage, LocalModelPhase,
    LocalModelSnapshot,
};
use agent_provider_config::{EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile};
#[cfg(test)]
use agent_provider_config::{ModelRequestLimits, ProviderProtocol};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::{self, Effect};
use ui::time::{Duration, Instant};
use ui::widget::{Button, ProgressBar, Row, Text, Widget};

use crate::effects::SettingsEffect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::panes::accounts::{BrowserOpener, PortalOpener};
use crate::panes::agent_vertex::{self, Polled, VertexFields, VertexSetup};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, SettingsSection, StatusTone, note, row, rows, section, section_with,
    semantic_status_pill, thing_row, value_row,
};
use ui::lock::MutexRecover as _;

#[cfg(test)]
mod admin_tests;
use provider_settings_ui::{ProviderEffect, ProviderPanel};

crate::effects::effect_family!(AgentModelsEffect => answer);

crate::area::pane_area! {
    AgentModelsArea;
    fn poll(&self) {
        self.poll_admin();
    }
    fn busy(&self) -> bool {
        self.admin_busy()
    }
}

/// The window's [`AgentModelsArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&AgentModelsArea> {
    app.areas().find()
}

pub use provider_settings_ui::AgentProviders;

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
    /// Reload the authoritative profiles and credential status.
    #[effect(job, device_only, label = "Refresh providers")]
    RefreshProviders,
    /// Add a hosted provider speaking `anthropic` or `openai-compatible`.
    #[effect(job, device_only, label = "Add provider")]
    AddProvider {
        #[param("`anthropic` or `openai-compatible`")]
        protocol: String,
    },
    /// Speak another protocol to a provider.
    #[effect(job, device_only, row, idempotent)]
    SetProviderProtocol {
        #[param("The provider's id")]
        provider: String,
        #[param("`anthropic` or `openai-compatible`")]
        protocol: String,
    },
    /// Offer a provider to agents, or stop offering it.
    #[effect(job, device_only, row, idempotent)]
    SetProviderEnabled {
        #[param("The provider's id")]
        provider: String,
        on: bool,
    },
    /// Name a provider, where it is, and the model agents use from it.
    #[effect(job, device_only, row)]
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
    #[effect(job, device_only, row, idempotent)]
    SetModelRequestLimits {
        provider: String,
        model: String,
        max_output_tokens: Option<u32>,
        runtime_context_tokens: Option<u32>,
        plan_timeout_ms: Option<u32>,
    },
    /// Read source context and this model's configured deployment limits.
    #[effect(device_only, row, returns, kind = "read")]
    InspectModelRequestLimits { provider: String, model: String },
    /// Make a provider the one agents use unless told otherwise.
    #[effect(job, device_only, row, idempotent, label = "Use by default")]
    SetDefaultProvider {
        #[param("The provider's id")]
        provider: String,
    },
    /// Remove a provider and its stored API key.
    #[effect(job, device_only, row, destructive, label = "Remove provider")]
    RemoveProvider {
        #[param("The provider's id")]
        provider: String,
    },
    /// Store a provider's API key in this device's vault: a hosted
    /// provider that roamed here from another device needs its own.
    #[effect(job, device_only, row, label = "Save API key")]
    SaveProviderKey {
        #[param("The provider's id")]
        provider: String,
        #[param(secret, "The API key; what the key field holds when omitted")]
        key: Option<String>,
    },
    /// Bind a legacy key only after independently confirming its original endpoint.
    #[effect(
        job,
        device_only,
        row,
        label = "Confirm original endpoint and migrate key"
    )]
    MigrateProviderKey {
        #[param("The provider's id")]
        provider: String,
        #[param("Original endpoint used with this key, not a newly edited endpoint")]
        original_endpoint: String,
    },
    /// Remove a legacy key that is not bound to a provider origin.
    #[effect(
        job,
        device_only,
        row,
        destructive,
        label = "Remove unbound legacy key"
    )]
    RemoveLegacyProviderKey {
        #[param("The provider's id")]
        provider: String,
    },
    /// Remove a provider's stored API key.
    #[effect(job, device_only, row, destructive, label = "Remove API key")]
    RemoveProviderKey {
        #[param("The provider's id")]
        provider: String,
    },
    /// Start adding Google Vertex AI, which signs in with Google instead of
    /// taking an API key. Shows its setup; signs in to nothing yet.
    #[effect(view, label = "Add Google Vertex AI")]
    AddGoogleVertex,
    /// Sign in with Google in the browser for Vertex AI: for the provider
    /// being added, or again for an existing one whose sign-in ended.
    #[effect(row, label = "Sign in with Google")]
    SignInWithGoogle {
        #[param("The provider's id; omit for the one being added")]
        provider: Option<String>,
    },
    /// Stop a Google sign-in, or the setup of a provider being added.
    #[effect(view)]
    CancelGoogleSignIn,
    /// Put one of the person's listed Google Cloud projects in the setup.
    #[effect(view)]
    ChooseVertexProject {
        #[param("The Google Cloud project id")]
        project: String,
    },
    /// Save the Vertex AI provider being added, once signed in, or change
    /// an existing one's project, location and model.
    #[effect(job, device_only, row)]
    SetVertexProvider {
        #[param("The provider's id; omit for the one being added")]
        provider: Option<String>,
        #[param("The Google Cloud project billed for requests")]
        project: String,
        #[param("The Vertex AI location; `global` when omitted")]
        location: Option<String>,
        #[param("The model, for example gemini-4-argon; the default when omitted")]
        model: Option<String>,
    },
}

/// Native sign-in actions augment the shared provider row at the same public door.
fn vertex_provider_row(row: LiveRow, profile: &ProviderProfile) -> LiveRow {
    row.operation(
        SettingsEffect::from(AgentModelsEffect::SignInWithGoogle {
            provider: Some(profile.id.clone()),
        })
        .bound(),
    )
    .operation(
        SettingsEffect::from(AgentModelsEffect::SetVertexProvider {
            provider: Some(profile.id.clone()),
            project: String::new(),
            location: None,
            model: None,
        })
        .bound_asking(&["project", "location", "model"]),
    )
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
    let shared = crate::panes::agent_models::area(app)
        .ok_or_else(crate::area::missing)?
        .model();
    let mut model = shared.borrow_mut();
    model.changed = true;
    match effect {
        AgentModelsEffect::InspectModelRequestLimits {
            provider,
            model: id,
        } => model.inspect_limits(&provider, &id).map(Effected::value),
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
            let shared = crate::panes::agent_models::area(app)
                .ok_or_else(crate::area::missing)?
                .model();
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
            Ok(crate::device_read::job(
                cx,
                // The manager is asked again on the job too: the answer
                // carries what the change left, read off the UI thread.
                move || {
                    let outcome = work.run(control.as_ref());
                    (outcome, control.snapshot())
                },
                |app: &mut SettingsApp,
                 (outcome, snapshot): (Result<String, String>, Snapshot),
                 _| {
                    let model = crate::panes::agent_models::area(app)
                        .ok_or_else(crate::area::missing)?
                        .model();
                    let mut model = model.borrow_mut();
                    model.snapshot_reads.supersede();
                    model.apply_snapshot(snapshot);
                    if let Err(error) = &outcome {
                        model.notice = Some(error.clone());
                    }
                    outcome.map(|_| Effected::done())
                },
            ))
        }
        AgentModelsEffect::AddGoogleVertex => {
            if model.vertex.as_ref().is_none_or(|setup| setup.existing) {
                model.cancel_vertex();
                model.vertex = Some(VertexSetup::adding());
            }
            Ok(Effected::done())
        }
        AgentModelsEffect::SignInWithGoogle { provider } => {
            model.sign_in_with_google(provider.as_deref())?;
            Ok(Effected::done())
        }
        AgentModelsEffect::CancelGoogleSignIn => {
            model.cancel_vertex();
            Ok(Effected::done())
        }
        AgentModelsEffect::ChooseVertexProject { project } => {
            model
                .vertex
                .as_ref()
                .filter(|setup| setup.choosing())
                .ok_or_else(|| t("agent-vertex-sign-in-first"))?
                .choose_project(&project);
            Ok(Effected::done())
        }
        provider => {
            let vertex_change = matches!(provider, AgentModelsEffect::SetVertexProvider { .. })
                || matches!(&provider, AgentModelsEffect::RemoveProvider { provider }
                    if model.panel.provider_config().profiles.iter().any(|profile| profile.id == *provider && profile.sign_in.is_some()));
            if vertex_change && model.cx.provider_admin.is_available() {
                model.submit_vertex_change(provider, cx.reply())?;
                Ok(Effected::Pending)
            } else if model.cx.provider_admin.is_available() {
                let provider = ProviderEffect::decode(provider.id(), &provider.arguments())?;
                model.submit_provider(provider, cx.reply())?;
                Ok(Effected::Pending)
            } else {
                if matches!(
                    provider,
                    AgentModelsEffect::SaveProviderKey { .. }
                        | AgentModelsEffect::RemoveProviderKey { .. }
                        | AgentModelsEffect::RemoveLegacyProviderKey { .. }
                        | AgentModelsEffect::MigrateProviderKey { .. }
                        | AgentModelsEffect::RefreshProviders
                ) {
                    let error = t("agent-provider-offline");
                    model.notice = Some(error.clone());
                    return Err(error);
                }
                let result = model.change_providers(provider);
                model.provider_notice(result.as_ref().map(|()| t("agent-provider-offline-saved")));
                result.map(|()| Effected::done())
            }
        }
    }
}

/// What the model manager reported, or why it could not.
type Snapshot = Result<LocalModelSnapshot, String>;

/// A model's reads of the device while its pane is open, made one at a
/// time off the UI thread and taken in at the sync their landing wakes.
struct Rereads<T> {
    read: crate::device_read::DeviceRead<T>,
    /// Asked for while a read ran: read once more when it lands, so the
    /// reading shows what changed meanwhile.
    again: bool,
    /// The read on its way began before a reading already taken in: it is
    /// dropped when it lands rather than shown over the newer one.
    stale: bool,
}

impl<T> Default for Rereads<T> {
    fn default() -> Self {
        Self {
            read: crate::device_read::DeviceRead::default(),
            again: false,
            stale: false,
        }
    }
}

impl<T: Send + 'static> Rereads<T> {
    /// Whether a read is on its way.
    fn running(&self) -> bool {
        !self.read.is_finished()
    }

    /// The reading that landed since last asked, unless a newer one was
    /// taken in meanwhile.
    fn landed(&mut self) -> Option<T> {
        if self.running() {
            return None;
        }
        let stale = std::mem::take(&mut self.stale);
        let reading = self.read.take()?;
        (!stale).then_some(reading)
    }

    /// Whether a read asked for while one ran is due now.
    fn due_again(&mut self) -> bool {
        let due = self.again && !self.running();
        if due {
            self.again = false;
        }
        due
    }

    /// Read with `read` off the UI thread, or once more after the read on
    /// its way. Returns a reading to take in now: one that had landed, or,
    /// on a host that runs no background work, the one read here.
    fn ask(&mut self, read: impl FnOnce() -> T + Clone + Send + 'static) -> Option<T> {
        if self.running() {
            self.again = true;
            return None;
        }
        // Finished stays finished until the next start: what landed is
        // taken here, so the start below is not refused for it.
        let landed = self.landed();
        // This read answers what was asked for while the last one ran.
        self.again = false;
        if self.read.start(read.clone()) {
            landed
        } else {
            Some(read())
        }
    }

    /// A reading made after the read on its way began was taken in.
    fn supersede(&mut self) {
        if self.running() {
            self.stale = true;
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
    /// The manager's snapshot, asked off the UI thread after the first frame.
    /// Provider administration is separately polled as a portable future.
    read: crate::device_read::DeviceRead<AgentModelsRead>,
}

/// The model manager's device snapshot plus unavailable credential statuses
/// until the host administration capability answers.
pub(crate) struct AgentModelsRead {
    snapshot: Result<LocalModelSnapshot, String>,
    keys: HashMap<String, Result<CredentialStatus, String>>,
}

impl AgentModelsRead {
    /// Ask only the device manager here; provider reads require host authority.
    fn of(control: &dyn LocalModelControl, hosted: Vec<String>) -> Self {
        Self {
            snapshot: control.snapshot(),
            keys: hosted
                .into_iter()
                .map(|id| (id, Err(t("agent-provider-offline"))))
                .collect(),
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

    /// The model, using the device manager's read or a screenshot stand-in.
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

    /// Ask the device manager off the UI thread; provider reads are futures.
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
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    pub(crate) fn poll_admin(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_admin();
        }
    }

    pub(crate) fn admin_busy(&self) -> bool {
        self.model
            .get()
            .is_some_and(|model| model.borrow().admin_pending.is_some())
    }

    /// One row per installed model, per provider, and the recommendation.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        self.model().borrow_mut().poll_admin();
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
        rows.extend(model.panel.rows().into_iter().map(|row| {
            let profile = model
                .panel
                .provider_config()
                .profiles
                .iter()
                .find(|profile| {
                    profile.sign_in.is_some()
                        && row.id() == format!("settings.agent-provider:{}", profile.id)
                });
            if let Some(profile) = profile {
                vertex_provider_row(row, profile)
            } else {
                row
            }
        }));
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
    last_poll: Instant,
    panel: ProviderPanel<PaneCx>,
    /// Manager reads remain native and off the UI thread; providers use the shared panel.
    snapshot_reads: Rereads<Snapshot>,
    vertex: Option<VertexSetup>,
    vertex_fields: Mutex<HashMap<String, VertexFields>>,
    vertex_keys: HashMap<String, Result<bool, String>>,
    vertex_reads: Rereads<HashMap<String, Result<bool, String>>>,
    custodian: account_custody::Custodian,
    opener: Arc<dyn BrowserOpener>,
    /// A native sign-in edit waiting for the authoritative provider acknowledgement.
    vertex_commit: Option<(String, Option<ProviderProfile>)>,
}

impl std::ops::Deref for AgentModelsModel {
    type Target = ProviderPanel<PaneCx>;
    fn deref(&self) -> &Self::Target {
        &self.panel
    }
}
impl std::ops::DerefMut for AgentModelsModel {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.panel
    }
}
impl AgentModelsModel {
    /// The model over `control`, starting the host's asynchronous provider read.
    fn new(cx: PaneCx, control: Arc<dyn LocalModelControl>) -> Self {
        let hosted = hosted_profiles(cx.follow::<AgentProviders>().get());
        let read = AgentModelsRead::of(control.as_ref(), hosted);
        Self::from_read(cx, control, read)
    }

    /// Build from the device snapshot. No legacy local settings are treated
    /// as authoritative when this mount has a provider administration grant.
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
        let panel = ProviderPanel::new(
            cx.clone(),
            cx.provider_admin.clone(),
            cx.follow::<AgentProviders>().get().config.clone(),
        );
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
            last_poll: ui::time::now(),
            panel,
            snapshot_reads: Rereads::default(),
            vertex: None,
            vertex_fields: Mutex::new(HashMap::new()),
            vertex_keys: HashMap::new(),
            vertex_reads: Rereads::default(),
            custodian: account_custody::Custodian::live(),
            opener: Arc::new(PortalOpener),
            vertex_commit: None,
        };
        model.notice = notice;
        model.sync_local_profile();
        model.keys = read.keys;
        model.read_keys();
        model.read_vertex_keys();
        model
    }

    /// Read native sign-in status off the UI thread, separately from API-key custody.
    fn read_vertex_keys(&mut self) {
        let ids: Vec<_> = self
            .panel
            .provider_config()
            .profiles
            .iter()
            .filter(|profile| profile.sign_in.is_some())
            .map(|profile| profile.id.clone())
            .collect();
        let custodian = self.custodian.clone();
        if let Some(keys) = self.vertex_reads.ask(move || {
            ids.into_iter()
                .map(|id| {
                    let result = custodian
                        .oauth_grant_state(&agent_vertex::grant_id(&id))
                        .map(|state| state == account_custody::GrantState::Usable)
                        .map_err(|error| error.to_string());
                    (id, result)
                })
                .collect()
        }) {
            self.vertex_keys = keys;
            self.changed = true;
        }
    }

    fn take_reads(&mut self) {
        if let Some(snapshot) = self.snapshot_reads.landed() {
            self.apply_snapshot(snapshot);
        }
        if self.snapshot_reads.due_again() {
            self.refresh();
        }
        if let Some(keys) = self.vertex_reads.landed() {
            self.vertex_keys = keys;
            self.changed = true;
        }
        if self.vertex_reads.due_again() {
            self.read_vertex_keys();
        }
    }

    /// Open Google's consent page for Vertex AI: for the provider being
    /// added when `provider` is `None`, else again for that provider.
    fn sign_in_with_google(&mut self, provider: Option<&str>) -> Result<(), String> {
        let current = self.vertex.take();
        let reuse = |setup: &VertexSetup| match provider {
            None => !setup.existing,
            Some(id) => setup.profile_id == id,
        };
        let mut setup = match current {
            Some(setup) if reuse(&setup) => setup,
            other => {
                self.vertex = other;
                self.cancel_vertex();
                match provider {
                    None => VertexSetup::adding(),
                    Some(id) => VertexSetup::again(
                        self.panel
                            .provider_config()
                            .profiles
                            .iter()
                            .find(|profile| profile.id == id && profile.signs_in_with_google())
                            .ok_or_else(|| t("agent-provider-gone"))?,
                    ),
                }
            }
        };
        setup.begin(&self.custodian, &self.opener);
        self.vertex = Some(setup);
        self.changed = true;
        Ok(())
    }

    /// Stop the Vertex AI sign-in or setup. A provider that was being added
    /// leaves no grant behind.
    fn cancel_vertex(&mut self) {
        if let Some(mut setup) = self.vertex.take() {
            setup.cancel();
            if !setup.existing {
                // Nothing may have been sealed yet; there is nothing to report.
                let _ = self
                    .custodian
                    .forget_oauth(&agent_vertex::grant_id(&setup.profile_id));
            }
        }
        self.changed = true;
    }

    fn vertex_fields(&self, profile: &ProviderProfile) -> VertexFields {
        self.vertex_fields
            .lock_recover()
            .entry(profile.id.clone())
            .or_insert_with(|| VertexFields::of(profile))
            .clone()
    }

    /// Ask the manager again off the UI thread; its answer is taken in at
    /// the sync its landing wakes ([`Self::take_reads`]).
    fn refresh(&mut self) {
        let control = Arc::clone(&self.control);
        if let Some(snapshot) = self.snapshot_reads.ask(move || control.snapshot()) {
            self.apply_snapshot(snapshot);
        }
    }

    /// Take in what the manager reported, or why it could not.
    fn apply_snapshot(&mut self, snapshot: Snapshot) {
        match snapshot {
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
        if self.cx.provider_admin.is_available() || !self.follow_local_profile {
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
            self.panel.offline_config = self.providers.get().config.clone();
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

    /// Forget a signed-in provider locally. API-key providers are removed through
    /// the shared administration capability, never through a UI-thread socket.
    fn remove_vertex_provider(
        &mut self,
        config: &mut ProviderConfig,
        provider: &str,
    ) -> Result<(), String> {
        let profile = config
            .profiles
            .iter()
            .find(|profile| profile.id == provider)
            .ok_or_else(|| t("agent-provider-gone"))?;
        if profile.sign_in.is_none() {
            return Err(t("agent-provider-offline"));
        }
        let _ = config.remove(provider);
        // Forgotten here only: revoking at Google would end every grant of
        // Tilo's client for this person, Mail's included.
        if self
            .vertex
            .as_ref()
            .is_some_and(|setup| setup.profile_id == provider)
        {
            self.cancel_vertex();
        }
        self.custodian
            .forget_oauth(&agent_vertex::grant_id(provider))
            .map_err(|error| {
                t_args(
                    "agent-sign-in-forget-err",
                    &[("error", error.to_string().into())],
                )
            })?;
        self.vertex_fields.lock_recover().remove(provider);
        Ok(())
    }

    /// Add the Vertex AI provider being set up (`provider` `None`), once
    /// signed in, or change an existing one; the provider's id.
    fn set_vertex_provider(
        &self,
        config: &mut ProviderConfig,
        provider: Option<String>,
        fields: (&str, Option<&str>, Option<&str>),
    ) -> Result<String, String> {
        let (id, existing) = match provider {
            Some(id) => (id, true),
            None => (
                self.vertex
                    .as_ref()
                    .filter(|setup| !setup.existing)
                    .map(|setup| setup.profile_id.clone())
                    .ok_or_else(|| t("agent-vertex-sign-in-first"))?,
                false,
            ),
        };
        if !existing && !agent_vertex::signed_in(&self.custodian, &id) {
            return Err(t("agent-vertex-sign-in-first"));
        }
        agent_vertex::apply(config, &id, existing, fields)?;
        Ok(id)
    }

    /// Close the setup of the Vertex AI provider `id` once it is saved, and
    /// read its fields and sign-in again.
    fn vertex_saved(&mut self, id: &str) {
        if self
            .vertex
            .as_ref()
            .is_some_and(|setup| setup.profile_id == id)
        {
            self.vertex = None;
        }
        self.vertex_fields.lock_recover().remove(id);
        self.read_keys();
        self.read_vertex_keys();
    }

    /// Keep native OAuth setup on the same borrowed administration capability as the panel.
    fn submit_vertex_change(
        &mut self,
        effect: AgentModelsEffect,
        reply: ui::framework::EffectReply,
    ) -> Result<(), String> {
        if self.panel.admin_pending.is_some() {
            return Err(t("agent-provider-busy"));
        }
        let snapshot = self
            .panel
            .admin_snapshot
            .as_ref()
            .ok_or_else(|| t("agent-provider-refresh-required"))?;
        let mut config = snapshot.config.clone();
        let id = match effect {
            AgentModelsEffect::SetVertexProvider {
                provider,
                project,
                location,
                model,
            } => self.set_vertex_provider(
                &mut config,
                provider,
                (&project, location.as_deref(), model.as_deref()),
            )?,
            AgentModelsEffect::RemoveProvider { provider } => {
                config
                    .remove(&provider)
                    .ok_or_else(|| t("agent-provider-gone"))?;
                provider
            }
            _ => return Err("not a sign-in provider change".into()),
        };
        let config = config.sanitised();
        let expected = config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned();
        self.panel.submit_config(config, reply)?;
        self.vertex_commit = Some((id, expected));
        self.changed = true;
        Ok(())
    }

    fn change_providers(&mut self, effect: AgentModelsEffect) -> Result<(), String> {
        // Offline metadata remains usable, but API-key custody is never simulated.
        self.panel.offline_config = self.providers.get().config.clone();
        let mut vertex_saved = None;
        let mut limits_ack = None;
        let config = match effect {
            AgentModelsEffect::SetVertexProvider {
                provider,
                project,
                location,
                model,
            } => {
                let mut config = self.providers.get().config.clone();
                vertex_saved = Some(self.set_vertex_provider(
                    &mut config,
                    provider,
                    (&project, location.as_deref(), model.as_deref()),
                )?);
                config.sanitised()
            }
            AgentModelsEffect::RemoveProvider { provider }
                if self
                    .providers
                    .get()
                    .config
                    .profiles
                    .iter()
                    .any(|profile| profile.id == provider && profile.sign_in.is_some()) =>
            {
                let mut config = self.providers.get().config.clone();
                self.remove_vertex_provider(&mut config, &provider)?;
                config.sanitised()
            }
            effect => {
                let effect = ProviderEffect::decode(effect.id(), &effect.arguments())?;
                limits_ack = self.remember_limits(&effect);
                self.proposed_config(effect)?
            }
        };
        self.providers
            .settings()
            .update(|providers| providers.config = config)
            .map_err(|error| error.to_string())?;
        self.providers.refresh();
        self.panel.offline_config = self.providers.get().config.clone();
        if let Some(id) = vertex_saved {
            self.vertex_saved(&id);
        }
        if let Some(acknowledgement) = limits_ack {
            self.acknowledge_limits(&acknowledgement);
        }
        self.changed = true;
        Ok(())
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
        Some(section_with(
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
        let fit_label = fit_label(hardware.fit);
        let mem = gib(i64::from(hardware.memory_gib));
        let hw_mem = t("agent-hw-memory");
        let hw_accel = t("agent-hw-accel");
        // How well this device fits is the group's badge, not a row too.
        let mut body = vec![
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
            return section_with(
                &t("agent-rec-title"),
                semantic_status_pill(&t("agent-service-unavailable"), StatusTone::Warning),
                rows(body),
            );
        };
        if self.reviewing.with(|reviewing| *reviewing) {
            body.extend([
                value_row(&t("agent-rec-model"), &package.name),
                note(&package.description),
            ]);
            self.push_recommendation_confirm(&mut body, package);
        } else {
            // The model this device is suited for, and setting it up, on one
            // row.
            let setup = t("agent-setup-device");
            body.push(row(Box::new(thing_row(
                "agent-models.recommended",
                &package.name,
                std::slice::from_ref(&package.description),
                Some(Box::new(
                    self.cx.bind_invocation(
                        Button::new("agent-models-setup-local", &setup)
                            .role(ui::widget::ButtonRole::Primary)
                            .with_enabled(
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
            ))));
        }
        section_with(
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
            let installable = model.installable();
            let removable = model.removable();
            if installable {
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
            }
            if removable {
                buttons = buttons.push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("agent-models-inprocess-remove-{widget_token}"),
                        &action_label(ModelAction::Remove),
                    ),
                    AgentModelsEffect::RemoveModel {
                        model: package.id.clone(),
                    },
                )));
            }
            if installable || removable {
                body.push(row(Box::new(buttons)));
            }
        }
        let (status, tone) = match &model.phase {
            None => (t("agent-status-not-installed"), StatusTone::Neutral),
            Some(LocalModelPhase::Ready { .. }) => (t("agent-status-ready"), StatusTone::Success),
            Some(phase) => (phase_status(phase), phase_tone(phase)),
        };
        section_with(
            &capability_title(package),
            semantic_status_pill(&status, tone),
            rows(body),
        )
    }

    fn model_card(&self, model: &InstalledLocalModel) -> SettingsSection {
        let status = phase_status(&model.phase);
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
        section_with(
            &model.package.name,
            semantic_status_pill(&status, phase_tone(&model.phase)),
            rows(body),
        )
    }

    /// The offer to make `profile` the default, when it has a model.
    fn default_button(
        &self,
        profile: &ProviderProfile,
        default_profile: Option<&str>,
    ) -> Option<Item> {
        profile.default_model.as_ref()?;
        Some(row(Box::new(self.cx.bind_invocation(
            Button::new(
                &format!("agent-models-default-{}", profile.id),
                &if default_profile == Some(profile.id.as_str()) {
                    t("agent-default-current")
                } else {
                    t("agent-use-default")
                },
            ),
            AgentModelsEffect::SetDefaultProvider {
                provider: profile.id.clone(),
            },
        ))))
    }

    /// A provider that signs in with Google: its project, location and
    /// model, whether its sign-in still holds on this device, and the offer
    /// to sign in again. It has no key, endpoint or protocol to edit.
    fn push_sign_in_profile(
        &self,
        body: &mut Vec<Item>,
        profile: &ProviderProfile,
        default_profile: Option<&str>,
    ) {
        let profile_id = profile.id.clone();
        self.vertex_fields(profile)
            .push(&self.cx, body, &profile_id);
        let setup = self
            .vertex
            .as_ref()
            .filter(|setup| setup.profile_id == profile_id);
        let waiting = setup.is_some_and(VertexSetup::waiting);
        let state = self.vertex_keys.get(&profile_id);
        if let Some(Err(error)) = state {
            body.push(row(crate::widgets::error_caption(&t_args(
                "agent-sign-in-read-error",
                &[("error", error.as_str().into())],
            ))));
        }
        // why: a sign-in status sentence or badge is a status line, not a label/control pair.
        body.push(row(if waiting {
            Box::new(Text::new(&t("agent-vertex-waiting")).dimmed())
        } else if matches!(state, Some(Ok(true))) {
            Box::new(Text::new(&t("agent-signed-in")).dimmed())
        } else if matches!(state, Some(Ok(false))) {
            semantic_status_pill(&t("agent-sign-in-again-needed"), StatusTone::Warning)
        } else {
            Box::new(Text::new(&t("agent-sign-in-unknown")).dimmed())
        }));
        if let Some(message) = setup.and_then(VertexSetup::message) {
            body.push(row(crate::widgets::error_caption(message)));
        }
        let sign_in: Box<dyn Widget> = if waiting {
            Box::new(self.cx.bind_invocation(
                Button::new(
                    &format!("agent-models-vertex-cancel-{profile_id}"),
                    &t("agent-cancel"),
                ),
                AgentModelsEffect::CancelGoogleSignIn,
            ))
        } else {
            Box::new(self.cx.bind_invocation(
                Button::new(
                    &format!("agent-models-vertex-sign-in-{profile_id}"),
                    &t("agent-sign-in-again"),
                ),
                AgentModelsEffect::SignInWithGoogle {
                    provider: Some(profile_id.clone()),
                },
            ))
        };
        body.push(row(Box::new(
            Row::new()
                .push(sign_in)
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
        body.extend(self.default_button(profile, default_profile));
    }

    /// The setup of a Vertex AI provider being added, then the hosted
    /// providers and their request limits.
    fn push_provider_cards(&self, cards: &mut Vec<SettingsSection>) {
        if let Some(setup) = self.vertex.as_ref().filter(|setup| !setup.existing) {
            cards.push(setup.section(&self.cx));
        }
        cards.extend(self.panel.sections_without_sign_in());
        let config = self.panel.provider_config();
        let mut body = vec![row(Box::new(self.cx.bind_invocation(
            Button::new("agent-models-add-vertex", &t("agent-add-vertex")),
            AgentModelsEffect::AddGoogleVertex,
        )))];
        for profile in config
            .profiles
            .iter()
            .filter(|profile| profile.sign_in.is_some())
        {
            self.push_sign_in_profile(&mut body, profile, config.default_profile.as_deref());
        }
        cards.push(section(&t("agent-vertex-title"), rows(body)));
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let private_title = t("agent-private-title");
        let local_inf = t("agent-local-inference");
        let local_gw = t("agent-local-gateway");
        let downloads = t("agent-downloads");
        let downloads_v = t("agent-downloads-value");
        let mut cards = vec![section(
            &private_title,
            rows(vec![
                note(&t("agent-private-caption")),
                value_row(&local_inf, &local_gw),
                value_row(&downloads, &downloads_v),
            ]),
        )];
        if !self.manager_available {
            cards.push(section(
                &t("agent-service-down-title"),
                rows(vec![note(&t("agent-service-down-body"))]),
            ));
            self.push_provider_cards(&mut cards);
            return crate::widgets::page(cards);
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
            cards.push(section(
                &t("agent-models-empty-title"),
                rows(vec![note(&t("agent-models-empty"))]),
            ));
        } else {
            for model in &self.snapshot.models {
                cards.push(self.model_card(model));
            }
        }
        self.push_provider_cards(&mut cards);
        crate::widgets::page(cards)
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

/// The fit's label, as the hardware card shows it.
fn fit_label(fit: HardwareFit) -> String {
    t(match fit {
        HardwareFit::Excellent => "agent-fit-excellent",
        HardwareFit::Good => "agent-fit-good",
        HardwareFit::Tight => "agent-fit-tight",
        HardwareFit::Unsupported => "agent-fit-unsupported",
    })
}

/// The card title naming what an in-process model does for the person.
fn capability_title(package: &LocalModelPackage) -> String {
    if package.segmentation {
        t("agent-inprocess-segmentation-title")
    } else {
        package.name.clone()
    }
}

/// The phase's status line.
fn phase_status(phase: &LocalModelPhase) -> String {
    t(match phase {
        LocalModelPhase::Downloading { .. } => "agent-status-downloading",
        LocalModelPhase::Installing { .. } => "agent-status-installing",
        LocalModelPhase::Ready { running: true, .. } => "agent-status-running",
        LocalModelPhase::Ready { running: false, .. } => "agent-status-ready",
        LocalModelPhase::Error { .. } => "agent-status-attention",
    })
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
        crate::device_read::off_ui_thread();
        let output = manager_run(&["snapshot", "--format", "json"], Duration::from_secs(10))?;
        if !output.status.success() {
            return Err(output.stderr_diagnostic());
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("model service returned invalid status: {error}"))
    }

    fn install(&self, model: &LocalModelPackage) -> Result<(), String> {
        crate::device_read::off_ui_thread();
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
        crate::device_read::off_ui_thread();
        Self::command("start", id)
    }
    fn stop(&self, id: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Self::command("stop", id)
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Self::command("remove", id)
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
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
    // A model is a download stored on the drive: a size.
    ui::format::size(bytes)
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
        self.take_reads();
        let revision = self
            .panel
            .admin_snapshot
            .as_ref()
            .map(|snapshot| snapshot.revision.clone());
        self.poll_admin();
        if revision
            != self
                .panel
                .admin_snapshot
                .as_ref()
                .map(|snapshot| snapshot.revision.clone())
        {
            self.read_vertex_keys();
        }
        if self.panel.admin_pending.is_none()
            && let Some((id, expected)) = self.vertex_commit.take()
            && self.panel.admin_snapshot.as_ref().is_some_and(|snapshot| {
                snapshot
                    .config
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
                    == expected.as_ref()
            })
        {
            if expected.is_some() {
                self.vertex_saved(&id);
            } else {
                if let Err(error) = self.custodian.forget_oauth(&agent_vertex::grant_id(&id)) {
                    self.notice = Some(t_args(
                        "agent-sign-in-forget-err",
                        &[("error", error.to_string().into())],
                    ));
                }
                if self
                    .vertex
                    .as_ref()
                    .is_some_and(|setup| setup.profile_id == id)
                {
                    self.cancel_vertex();
                }
                self.vertex_fields.lock_recover().remove(&id);
                self.read_vertex_keys();
            }
        }
        if !self.cx.provider_admin.is_available() && self.providers.refresh() {
            self.panel.offline_config = self.providers.get().config.clone();
            self.read_keys();
            self.read_vertex_keys();
        }
        let polled = self.vertex.as_mut().map(VertexSetup::poll);
        match polled {
            None | Some(Polled::Nothing) => {}
            Some(Polled::Changed) => self.changed = true,
            Some(Polled::SignedInAgain) => {
                self.vertex = None;
                self.read_keys();
                self.read_vertex_keys();
                self.notice = Some(t("agent-vertex-signed-in"));
            }
        }
        // While a model is busy the manager is asked again, one read at a
        // time, each a while after the last landed.
        if self.is_busy()
            && !self.snapshot_reads.running()
            && self.last_poll.elapsed() >= POLL_INTERVAL
        {
            self.refresh();
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "AgentModelsPane"
    }

    fn busy(&self) -> bool {
        self.admin_pending.is_some()
            || self.is_busy()
            || self.snapshot_reads.running()
            || self.vertex_reads.running()
            || self.vertex.as_ref().is_some_and(VertexSetup::waiting)
    }
}

/// The model manager a screenshot and the crate's tests show: a good fit,
/// one recommended model, nothing installed. Every request succeeds.
struct SampleManager;

impl LocalModelControl for SampleManager {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        crate::device_read::off_ui_thread();
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
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn start(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn stop(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
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
        pane.keys
            .insert(profile.id.clone(), Ok(CredentialStatus::Missing));
        assert!(pane.needs_key(&profile));
        let text = crate::widgets::announced_text(pane.compose().as_ref());
        assert!(
            text.iter().any(|line| line == "Needs a key on this device"),
            "{text:#?}"
        );

        pane.keys
            .insert(profile.id.clone(), Ok(CredentialStatus::Configured));
        assert!(!pane.needs_key(&profile));
        let save = SettingsEffect::from(AgentModelsEffect::SaveProviderKey {
            provider: profile.id,
            key: None,
        })
        .bound_asking(&["key"]);
        assert!(save.params().iter().any(|param| param.name == "key"));
    }

    #[test]
    fn a_legacy_key_requires_an_explicit_endpoint_and_offers_removal() {
        let mut pane = model_over(mock(snapshot(None)));
        pane.change_providers(AgentModelsEffect::AddProvider {
            protocol: "anthropic".into(),
        })
        .unwrap();
        let profile = pane.providers.get().config.profiles[0].clone();
        pane.keys
            .insert(profile.id.clone(), Ok(CredentialStatus::MigrationRequired));
        assert!(pane.needs_key(&profile));
        let text = crate::widgets::announced_text(pane.compose().as_ref());
        assert!(
            text.iter()
                .any(|line| line.contains("unbound legacy API key")),
            "{text:#?}"
        );
        assert!(
            text.iter().any(|line| line == "Remove unbound legacy key"),
            "{text:#?}"
        );
        let migration = SettingsEffect::from(AgentModelsEffect::MigrateProviderKey {
            provider: profile.id,
            original_endpoint: String::new(),
        })
        .bound_asking(&["original_endpoint"]);
        assert!(
            migration
                .params()
                .iter()
                .any(|param| param.name == "original_endpoint")
        );
    }

    /// Google Vertex AI is added only once signed in, with the project,
    /// location and model chosen; it shows its sign-in rather than a key,
    /// and removing it forgets its own grant and no other.
    #[test]
    fn a_vertex_provider_signs_in_and_forgets_only_its_own_grant() {
        use account_custody::TokenSet;
        use account_custody::cloud_projects::CloudProject;
        let vault = tempfile::tempdir().unwrap();
        let mut pane = model_over(mock(snapshot(None)));
        pane.custodian = account_custody::Custodian::for_root(Some(vault.path().to_path_buf()));
        let grant = |refresh: &str| TokenSet {
            access_token: "access".into(),
            scope: Some("https://www.googleapis.com/auth/cloud-platform".into()),
            refresh_token: refresh.into(),
            expires_in: 3600,
            obtained_at: 0,
        };
        let mail = accounts::AccountId::new("ada@example.com");
        pane.custodian
            .seal_oauth(&mail, &grant("mail-refresh"))
            .unwrap();

        pane.vertex = Some(VertexSetup::adding());
        let id = pane.vertex.as_ref().unwrap().profile_id.clone();
        let add = |project: &str| AgentModelsEffect::SetVertexProvider {
            provider: None,
            project: project.into(),
            location: None,
            model: None,
        };
        assert_eq!(
            pane.change_providers(add("tilo-509618")),
            Err(t("agent-vertex-sign-in-first")),
            "nothing is saved before the sign-in"
        );
        pane.custodian
            .seal_oauth(&agent_vertex::grant_id(&id), &grant("vertex-refresh"))
            .unwrap();
        pane.vertex
            .as_mut()
            .unwrap()
            .signed_in_for_test(vec![CloudProject {
                id: "tilo-509618".into(),
                name: "Tilo".into(),
            }]);
        let text = crate::widgets::announced_text(pane.compose().as_ref());
        assert!(
            text.iter().any(|line| line == "Tilo (tilo-509618)"),
            "{text:#?}"
        );
        assert_eq!(
            pane.change_providers(add("Not A Project")),
            Err(t("agent-vertex-project-invalid"))
        );
        assert!(pane.providers.get().config.profiles.is_empty());

        pane.change_providers(add("tilo-509618")).unwrap();
        assert!(pane.vertex.is_none(), "the setup ends once saved");
        let stored = pane.cx.settings::<AgentProviders>().get().config;
        let profile = &stored.profiles[0];
        assert_eq!(profile.id, id, "the grant was sealed for this provider");
        assert!(profile.signs_in_with_google());
        assert!(profile.credential.is_none());
        assert_eq!(
            profile.endpoint,
            "https://aiplatform.googleapis.com/v1/projects/tilo-509618/locations/global/endpoints/openapi"
        );
        assert_eq!(
            profile.sign_in.as_ref().unwrap().billing_project.as_deref(),
            Some("tilo-509618")
        );
        assert_eq!(
            profile.default_model.as_deref(),
            Some("google/gemini-4-argon")
        );
        assert!(stored.validation_errors().is_empty());

        shows_its_sign_in_and_forgets_only_its_grant(&mut pane, &id, &mail);
    }

    fn shows_its_sign_in_and_forgets_only_its_grant(
        pane: &mut AgentModelsModel,
        id: &str,
        mail: &accounts::AccountId,
    ) {
        use account_custody::GrantState;
        let id = id.to_string();
        pane.vertex_keys.insert(id.clone(), Ok(false));
        let text = crate::widgets::announced_text(pane.compose().as_ref());
        assert!(text.iter().any(|line| line == "Sign in again"), "{text:#?}");
        assert!(!text.iter().any(|line| line == "Save key"), "{text:#?}");
        pane.vertex_keys.insert(id.clone(), Ok(true));
        let text = crate::widgets::announced_text(pane.compose().as_ref());
        assert!(
            text.iter()
                .any(|line| line == "Signed in with Google on this device."),
            "{text:#?}"
        );

        pane.change_providers(AgentModelsEffect::SetVertexProvider {
            provider: Some(id.clone()),
            project: "other-project".into(),
            location: Some("europe-west4".into()),
            model: Some("gemini-2.5-pro".into()),
        })
        .unwrap();
        let profile = pane.cx.settings::<AgentProviders>().get().config.profiles[0].clone();
        assert_eq!(
            profile.endpoint,
            "https://europe-west4-aiplatform.googleapis.com/v1/projects/other-project/locations/europe-west4/endpoints/openapi"
        );
        assert_eq!(
            profile.default_model.as_deref(),
            Some("google/gemini-2.5-pro")
        );
        assert_eq!(
            profile.sign_in.unwrap().billing_project.as_deref(),
            Some("other-project")
        );

        pane.change_providers(AgentModelsEffect::RemoveProvider {
            provider: id.clone(),
        })
        .unwrap();
        assert!(pane.providers.get().config.profiles.is_empty());
        assert_eq!(
            pane.custodian
                .oauth_grant_state(&agent_vertex::grant_id(&id))
                .unwrap(),
            GrantState::Missing
        );
        assert_eq!(
            pane.custodian.oauth_grant_state(mail).unwrap(),
            GrantState::Usable,
            "Mail's grant of the same person is untouched"
        );
    }

    /// Cancelling the setup of a provider being added leaves no grant.
    #[test]
    fn cancelling_a_vertex_setup_forgets_its_grant() {
        let vault = tempfile::tempdir().unwrap();
        let mut pane = model_over(mock(snapshot(None)));
        pane.custodian = account_custody::Custodian::for_root(Some(vault.path().to_path_buf()));
        pane.vertex = Some(VertexSetup::adding());
        let id = pane.vertex.as_ref().unwrap().profile_id.clone();
        pane.custodian
            .seal_oauth(
                &agent_vertex::grant_id(&id),
                &account_custody::TokenSet {
                    access_token: "a".into(),
                    scope: None,
                    refresh_token: "r".into(),
                    expires_in: 3600,
                    obtained_at: 0,
                },
            )
            .unwrap();
        pane.cancel_vertex();
        assert!(pane.vertex.is_none());
        assert!(!agent_vertex::signed_in(&pane.custodian, &id));
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
                endpoint: Some(agent_provider_config::local::LocalEndpoint {
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
            .areas()
            .find::<crate::panes::agent_models::AgentModelsArea>()
            .unwrap()
            .model()
            .borrow_mut()
            .follow_local_profile = false;
        let open = crate::categories::Category::AgentModels;
        app.effect_on(
            &crate::effects::pane_row_id(open),
            &crate::effects::open_pane(open),
        )
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

    /// A model manager that takes requests at once but does not report
    /// its snapshot until it is let go (or ten seconds pass), as a wedged
    /// `tilo-model-manager`. Once let go it reports a model ready.
    struct StalledManager(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

    /// A snapshot with one model called `name` in `phase`.
    fn one_model(name: &str, phase: LocalModelPhase) -> LocalModelSnapshot {
        LocalModelSnapshot {
            models: vec![InstalledLocalModel {
                package: LocalModelPackage {
                    name: name.into(),
                    ..package()
                },
                phase,
            }],
            ..snapshot(None)
        }
    }

    impl LocalModelControl for StalledManager {
        fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
            crate::device_read::off_ui_thread();
            let (open, released) = &*self.0;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut open = open.lock().unwrap();
            while !*open {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Err("the model manager never answered".into());
                }
                open = released.wait_timeout(open, left).unwrap().0;
            }
            drop(open);
            Ok(one_model(
                "Porch Model",
                LocalModelPhase::Ready {
                    running: false,
                    endpoint: None,
                },
            ))
        }
        fn install(&self, _: &LocalModelPackage) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn start(&self, _: &str) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn stop(&self, _: &str) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn remove(&self, _: &str) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn stage_accelerator(&self) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
    }

    /// A click is answered and the window keeps drawing while the model
    /// manager takes its time to report: neither the snapshot asked after
    /// the change nor the one a busy model's polling asks holds the window,
    /// and the snapshot lands once the manager answers.
    #[test]
    fn a_stalled_model_manager_never_holds_the_window() {
        use ui::framework::testing::TestApp;

        let mut app = TestApp::<crate::resource::SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::AgentModels);
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let stalled = Arc::clone(&gate);
        app.with_widget::<SettingsApp, _>(move |settings| {
            let model = crate::panes::agent_models::area(settings).unwrap().model();
            let mut model = model.borrow_mut();
            model.follow_local_profile = false;
            model.control = Arc::new(StalledManager(stalled));
            // A model downloading: the pane asks the manager again soon.
            model.snapshot = one_model(
                "Busy Model",
                LocalModelPhase::Downloading {
                    downloaded_bytes: 1,
                    total_bytes: 4,
                },
            );
            model.last_poll = ui::time::now()
                .checked_sub(POLL_INTERVAL)
                .unwrap_or_else(ui::time::now);
            model.changed = true;
        })
        .expect("the Settings window is mounted");

        let clicked = std::time::Instant::now();
        app.start_effect(&SettingsEffect::from(AgentModelsEffect::SetUpAccelerator))
            .expect("the accelerator setup runs as a job");
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        let answered = clicked.elapsed();
        assert!(
            answered < std::time::Duration::from_secs(1),
            "the click waited {answered:?} on the model manager"
        );
        let shown = |app: &mut TestApp<crate::resource::SettingsShell>| {
            app.with_widget::<SettingsApp, _>(|settings| {
                let model = crate::panes::agent_models::area(settings)
                    .unwrap()
                    .model()
                    .borrow();
                model
                    .snapshot
                    .models
                    .iter()
                    .map(|model| model.package.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap()
        };
        assert_eq!(
            shown(&mut app),
            ["Busy Model"],
            "the last snapshot is shown until the manager answers"
        );

        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while shown(&mut app) != ["Porch Model"] {
            assert!(
                std::time::Instant::now() < deadline,
                "the snapshot the manager reported is shown"
            );
            app.settle();
            std::thread::yield_now();
        }
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
