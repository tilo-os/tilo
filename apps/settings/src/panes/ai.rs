//! AI — the one hub for model providers, the models on this device, and
//! which model serves which task.
//!
//! The pane renders [`AiState`] and changes nothing itself: every control is
//! bound to an action of the hub's resource tree
//! (`crate::actions::ai_section_for`), so a person, an agent and
//! `tilo-control` configure the hub through the same doors, with the same
//! confirmations for keys and downloads. Local-model lifecycle commands run
//! here because the pane owns the model-manager client; Settings never
//! downloads model bytes or starts a server itself.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_provider_config::assign::{Layer, Preset, ResolveError, preset_plan};
use agent_provider_config::local::{AcceleratorState, RestartReason, SpeedClass};
use agent_provider_config::manager::{
    AcceleratorSetupState, AvailableModel, HardwareFit, InstalledLocalModel, LocalModelPackage,
    LocalModelPhase, LocalModelSnapshot,
};
use agent_provider_config::tasks::{self, PrivacyClass, TaskSpec};
use agent_provider_config::{
    Credential, EndpointLocation, ModelCapability, ProviderProfile, ProviderProtocol,
};
use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use tilo_actions::{ActionBinding, Resource, ResourcePresentation};
use tilo_model_gateway::{CredentialState, ProviderHealth, Reachability};
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, ProgressBar, Row, Spacer, Text, TextInput, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};
use zeroize::Zeroize;

use crate::ai_settings::{AiState, target_value};
use crate::i18n::{arg, t, t_args};
use crate::widgets::{
    El, StatusTone, caption, card, card_with_action, choice_row, hgap, rows, semantic_status_pill,
    setting_row, value_row,
};

pub const PROVIDER_RESOURCE_PREFIX: &str = "settings.ai-provider:";
pub const PROVIDER_MODEL_RESOURCE_PREFIX: &str = "settings.ai-provider-model:";
pub const TASK_RESOURCE_PREFIX: &str = "settings.ai-task:";
pub const AGENT_RESOURCE_PREFIX: &str = "settings.ai-agent:";
pub const RELEASE_RESOURCE_PREFIX: &str = "settings.ai-local-release:";
pub const INSTALLED_RESOURCE_PREFIX: &str = "settings.agent-model:";
/// The `choose-model` / `set-reasoning-effort` choice that clears a choice.
pub const DEFAULT_CHOICE: &str = "default";

const POLL_INTERVAL: Duration = Duration::from_millis(750);
const MANAGER_OUTPUT_LIMIT: usize = 1024 * 1024;

/// Bumped whenever an operation changes the person's AI state, so an open
/// pane re-reads it on its next update.
static REVISION: AtomicU64 = AtomicU64::new(0);
/// A person-readable result of the last background operation.
static NOTICE: Mutex<Option<String>> = Mutex::new(None);
/// The provider whose account sign-in is open in the browser.
static SIGNING_IN: Mutex<Option<String>> = Mutex::new(None);

/// Tell an open pane its state changed.
pub fn changed() {
    REVISION.fetch_add(1, Ordering::Relaxed);
}

/// Report the outcome of a background operation to the pane.
pub fn notify(message: String) {
    if let Ok(mut notice) = NOTICE.lock() {
        *notice = Some(message);
    }
    changed();
}

/// Mark an account sign-in as open (`Some`) or finished (`None`).
pub fn set_signing_in(profile: Option<String>) {
    if let Ok(mut signing) = SIGNING_IN.lock() {
        *signing = profile;
    }
    changed();
}

fn signing_in() -> Option<String> {
    SIGNING_IN.lock().ok().and_then(|signing| signing.clone())
}

#[must_use]
pub const fn capability_slug(capability: ModelCapability) -> &'static str {
    match capability {
        ModelCapability::Vision => "vision",
        ModelCapability::FaceRecognition => "face-recognition",
        ModelCapability::TraceReview => "trace-review",
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_073_741_824.0)
}

/// About how many words a second `tokens` per second reads as.
fn words_per_second(tokens: u16) -> u16 {
    tokens.saturating_mul(3) / 4
}

fn speed_label(available: &AvailableModel) -> String {
    let words = f64::from(words_per_second(available.speed.tokens_per_second));
    match available.speed.class {
        SpeedClass::Interactive => t("ai-speed-interactive"),
        SpeedClass::Slow => t_args("ai-speed-slow", &[("words", words.into())]),
        SpeedClass::Impractical => t("ai-speed-impractical"),
    }
}

/// The terms a download is confirmed against: size, license and where it
/// runs. Shown on the release row and as the install action's description.
#[must_use]
pub fn download_terms(available: &AvailableModel) -> String {
    let package = &available.package;
    t_args(
        "ai-download-terms",
        &[
            ("download", arg(gib(package.download_bytes))),
            ("disk", arg(gib(package.installed_bytes))),
            ("license", arg(package.license.as_str())),
            ("url", arg(package.license_url.as_str())),
            ("speed", arg(speed_label(available))),
        ],
    )
}

/// The resource-tree presentation of an installed model's phase.
#[must_use]
pub fn phase_presentation(phase: &LocalModelPhase) -> ResourcePresentation {
    ResourcePresentation {
        detail: Some(phase_detail(phase)),
        badge: Some(phase_status(phase)),
        preview: None,
    }
}

fn phase_status(phase: &LocalModelPhase) -> String {
    t(match phase {
        LocalModelPhase::Downloading { .. } => "ai-phase-downloading",
        LocalModelPhase::Installing { .. } => "ai-phase-installing",
        LocalModelPhase::Ready { running: true, .. } => "ai-phase-running",
        LocalModelPhase::Ready { running: false, .. } => "ai-phase-ready",
        LocalModelPhase::Error { .. } => "ai-phase-error",
    })
}

fn phase_detail(phase: &LocalModelPhase) -> String {
    match phase {
        LocalModelPhase::Downloading {
            downloaded_bytes,
            total_bytes,
        } => t_args(
            "ai-phase-downloading-detail",
            &[
                ("done", arg(gib(*downloaded_bytes))),
                ("total", arg(gib(*total_bytes))),
            ],
        ),
        LocalModelPhase::Installing { message, .. } | LocalModelPhase::Error { message } => {
            message.clone()
        }
        LocalModelPhase::Ready { running: true, .. } => t("ai-phase-running-detail"),
        LocalModelPhase::Ready { running: false, .. } => t("ai-phase-ready-detail"),
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

fn manager_run(args: &[&str], timeout: Duration) -> Result<bounded_command::Output, String> {
    bounded_command::run(
        "tilo-model-manager",
        args,
        None,
        MANAGER_OUTPUT_LIMIT,
        timeout,
    )
    .map_err(|error| format!("model service is unavailable: {error}"))
}

fn manager_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control() || matches!(character, ' ' | '\n' | '\t'))
        .take(512)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Boundary implemented by Tilo's model service. The production adapter is a
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
            return Err(manager_diagnostic(&output.stderr));
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

/// The manager's current, signed inventory, for the resource tree and for
/// effect-time validation. Performs no mutation.
///
/// # Errors
///
/// Returns the local manager's unavailable or malformed-snapshot error.
pub fn live_snapshot() -> Result<LocalModelSnapshot, String> {
    LocalModelControl::snapshot(&ModelManagerCli)
}

/// A control that answers with a fixed snapshot and refuses every change:
/// what a `--shots` render and the tests drive.
struct FixedControl(Result<LocalModelSnapshot, String>);

impl LocalModelControl for FixedControl {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        self.0.clone()
    }
    fn install(&self, _: &LocalModelPackage) -> Result<(), String> {
        Err("fixed snapshot".into())
    }
    fn start(&self, _: &str) -> Result<(), String> {
        Err("fixed snapshot".into())
    }
    fn stop(&self, _: &str) -> Result<(), String> {
        Err("fixed snapshot".into())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Err("fixed snapshot".into())
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        Err("fixed snapshot".into())
    }
}

/// Process-local text buffers for one provider's editable fields. They never
/// enter the collaborative document; the key buffer is wiped on drop.
#[derive(Clone, Default)]
struct ProviderBuffers {
    label: Arc<Mutex<String>>,
    endpoint: Arc<Mutex<String>>,
    key: Arc<Mutex<String>>,
    model: Arc<Mutex<String>>,
    context: Arc<Mutex<String>>,
}

fn read(buffer: &Arc<Mutex<String>>) -> String {
    buffer.lock().map(|value| value.clone()).unwrap_or_default()
}

pub struct AiPane {
    control: Arc<dyn LocalModelControl>,
    state: AiState,
    /// A `--shots` or test fixture: never re-read from disk.
    fixture: bool,
    revision: u64,
    notice: Option<String>,
    probed: Arc<Mutex<BTreeMap<String, ProviderHealth>>>,
    probing: bool,
    buffers: Arc<Mutex<HashMap<String, ProviderBuffers>>>,
    last_poll: Instant,
    structure_dirty: bool,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl AiPane {
    #[must_use]
    pub fn new() -> Self {
        let control: Arc<dyn LocalModelControl> = Arc::new(ModelManagerCli);
        let state = AiState::load(control.snapshot());
        let mut pane = Self::from_parts(control, state, false);
        pane.start_probe();
        pane
    }

    /// A pane over a fixed `state`, for `--shots` and tests.
    #[must_use]
    pub fn for_state(state: AiState) -> Self {
        let control = Arc::new(FixedControl(state.snapshot.clone()));
        Self::from_parts(control, state, true)
    }

    fn from_parts(control: Arc<dyn LocalModelControl>, state: AiState, fixture: bool) -> Self {
        Self {
            control,
            state,
            fixture,
            revision: REVISION.load(Ordering::Relaxed),
            notice: None,
            probed: Arc::new(Mutex::new(BTreeMap::new())),
            probing: false,
            buffers: Arc::new(Mutex::new(HashMap::new())),
            last_poll: Instant::now(),
            structure_dirty: false,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self
    }

    #[must_use]
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    ) -> Self {
        self.command_state = commands.clone();
        self
    }

    /// Ask the gateway, off the UI thread, whether each provider's
    /// credential is present and its endpoint answers.
    fn start_probe(&mut self) {
        if self.fixture || !tilo_model_gateway::available() {
            return;
        }
        let ids: Vec<String> = self
            .state
            .config
            .profiles
            .iter()
            .filter(|profile| !profile.is_model_manager())
            .map(|profile| profile.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        self.probing = true;
        let probed = Arc::clone(&self.probed);
        std::thread::spawn(move || {
            for id in ids {
                if let Ok(health) = tilo_model_gateway::probe(&id)
                    && let Ok(mut probed) = probed.lock()
                {
                    probed.insert(id, health);
                }
            }
            changed();
        });
    }

    fn refresh(&mut self, with_manager: bool) {
        if self.fixture {
            return;
        }
        let snapshot = if with_manager {
            self.control.snapshot()
        } else {
            self.state.snapshot.clone()
        };
        let health = std::mem::take(&mut self.state.health);
        self.state = AiState::load(snapshot);
        self.state.health = health;
        self.last_poll = Instant::now();
        self.structure_dirty = true;
    }

    fn is_busy(&self) -> bool {
        let local = self.state.snapshot.as_ref().is_ok_and(|snapshot| {
            snapshot
                .models
                .iter()
                .any(|model| model.phase.is_transient())
                || snapshot
                    .accelerator_setup
                    .as_ref()
                    .is_some_and(|setup| setup.status.is_transient())
        });
        local || signing_in().is_some() || self.probing
    }

    fn operation_finished(
        &self,
        operation: &crate::actions::AgentModelOperation,
        result: Result<String, String>,
    ) {
        let token = crate::actions::local_operation_token_for(
            crate::actions::AGENT_MODEL_OPERATION_TYPE,
            operation,
        );
        if !matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: active } if active == token
        ) {
            return;
        }
        self.operation_status.set(match result {
            Ok(message) => crate::actions::LocalOperationStatus::Succeeded { token, message },
            Err(message) => crate::actions::LocalOperationStatus::Failed { token, message },
        });
    }

    /// Run one admitted local-model command against a fresh snapshot.
    fn run_model_operation(&mut self, operation: &crate::actions::AgentModelOperation) {
        use crate::actions::AgentModelOperation as Op;
        let snapshot = self.control.snapshot();
        let package = |id: &str, revision: Option<&str>| {
            let snapshot = snapshot.as_ref().ok()?;
            snapshot
                .available
                .iter()
                .map(|available| &available.package)
                .chain(snapshot.models.iter().map(|model| &model.package))
                .find(|package| {
                    package.id == id && revision.is_none_or(|revision| package.revision == revision)
                })
                .cloned()
        };
        let result = match operation {
            Op::StageAccelerator => self.control.stage_accelerator(),
            Op::Install { id, revision } | Op::Retry { id, revision } => {
                package(id, Some(revision))
                    .ok_or_else(|| t("ai-notice-release-gone"))
                    .and_then(|package| self.control.install(&package))
            }
            Op::Start { id } => self.control.start(id),
            Op::Stop { id } => self.control.stop(id),
            Op::Remove { id } => self.control.remove(id),
        };
        match result {
            Ok(()) => {
                self.refresh(true);
                self.operation_finished(operation, Ok(t("ai-notice-accepted")));
            }
            Err(error) => {
                let message = t_args("ai-notice-failed", &[("error", arg(error))]);
                self.operation_finished(operation, Err(message.clone()));
                self.notice = Some(message);
                self.structure_dirty = true;
            }
        }
    }

    fn buffers_for(&self, profile: &ProviderProfile) -> ProviderBuffers {
        let mut buffers = self
            .buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        buffers
            .entry(profile.id.clone())
            .or_insert_with(|| ProviderBuffers {
                label: Arc::new(Mutex::new(profile.label.clone())),
                endpoint: Arc::new(Mutex::new(profile.endpoint.clone())),
                ..ProviderBuffers::default()
            })
            .clone()
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let tree = crate::actions::ai_section_for(&self.state);
        let mut sections: Vec<El> = vec![Self::intro_card()];
        if let Some(notice) = &self.notice {
            sections.push(card(&t("ai-notice-title"), caption(notice)));
        }
        sections.push(self.presets_card(&tree));
        sections.push(self.tasks_card(&tree));
        sections.push(self.agents_card(&tree));
        sections.push(self.providers_card(&tree));
        sections.push(self.device_card(&tree));
        crate::widgets::cards(sections)
    }

    fn intro_card() -> El {
        card(
            &t("ai-intro-title"),
            rows(vec![
                caption(&t("ai-intro-body")),
                value_row(&t("ai-intro-credentials"), &t("ai-intro-credentials-value")),
                value_row(&t("ai-intro-downloads"), &t("ai-intro-downloads-value")),
            ]),
        )
    }

    fn resolution_line(
        &self,
        task: &'static TaskSpec,
        agent: Option<&str>,
    ) -> (String, StatusTone) {
        match self.state.resolve(task, agent) {
            Ok(resolution) => {
                let layer = match resolution.layer {
                    Layer::Agent => t("ai-layer-agent"),
                    Layer::Task => t("ai-layer-task"),
                    Layer::Preset(preset) => {
                        t_args("ai-layer-preset", &[("preset", arg(t(preset.label_key())))])
                    }
                    Layer::Recommended => t("ai-layer-recommended"),
                };
                let mut line = t_args(
                    "ai-resolved",
                    &[
                        ("model", arg(resolution.model.display_label())),
                        ("provider", arg(provider_name(&resolution.profile))),
                        ("layer", arg(layer)),
                    ],
                );
                if resolution.options.fast_mode {
                    line.push_str(" · ");
                    line.push_str(&t("ai-feature-fast-mode"));
                }
                if let Some(effort) = resolution.options.reasoning_effort {
                    line.push_str(" · ");
                    line.push_str(&t(effort.label_key()));
                }
                (line, StatusTone::Success)
            }
            Err(error) => (resolve_error_text(&error), StatusTone::Warning),
        }
    }

    fn presets_card(&self, tree: &Resource) -> El {
        let current = self.state.assignments.preset;
        let mut body = vec![caption(&t("ai-presets-body"))];
        for preset in Preset::ALL {
            let plan = preset_plan(&self.state.inputs(), preset);
            let in_use = current == Some(preset);
            let action = format!("preset-{}", preset.slug());
            let button = bound_button(
                tree,
                None,
                &action,
                &format!("ai.{action}"),
                &t(if in_use {
                    "ai-preset-in-use"
                } else {
                    "ai-preset-use"
                }),
            )
            .with_enabled(!in_use);
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Text::new(&t(preset.label_key())).with_scaled_size(14.0),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(if in_use {
                        semantic_status_pill(&t("ai-preset-in-use"), StatusTone::Accent)
                    } else {
                        Box::new(button)
                    }),
            ));
            body.push(caption(&t(preset.description_key())));
            for outcome in plan {
                let text = match &outcome.outcome {
                    Ok(resolution) => t_args(
                        "ai-preset-outcome",
                        &[
                            ("task", arg(t(outcome.task.label_key))),
                            ("model", arg(resolution.model.display_label())),
                            ("provider", arg(provider_name(&resolution.profile))),
                        ],
                    ),
                    Err(error) => t_args(
                        "ai-preset-outcome-missing",
                        &[
                            ("task", arg(t(outcome.task.label_key))),
                            ("reason", arg(resolve_error_text(error))),
                        ],
                    ),
                };
                body.push(caption(&text));
            }
        }
        if current.is_some() {
            body.push(Box::new(
                Row::new()
                    .push(Box::new(bound_button(
                        tree,
                        None,
                        "preset-none",
                        "ai.preset-none",
                        &t("ai-action-preset-none"),
                    )))
                    .push(Box::new(Spacer::new())),
            ));
        }
        card(&t("ai-presets-title"), rows(body))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one scope's model, options, off-device allowance and setup stay together"
    )]
    fn scope_controls(
        &self,
        tree: &Resource,
        resource_id: &str,
        task: &'static TaskSpec,
        agent: Option<&str>,
        body: &mut Vec<El>,
    ) {
        let Some(resource) = crate::actions::ai_resource(tree, Some(resource_id)) else {
            return;
        };
        let explicit = match agent {
            Some(agent) => self.state.assignments.agent(agent, task.id),
            None => self.state.assignments.tasks.get(task.id),
        };
        let selected = explicit.map_or_else(
            || DEFAULT_CHOICE.to_string(),
            |assignment| target_value(&assignment.target),
        );
        let off_device = task.privacy == PrivacyClass::Open;
        let mut options = vec![(
            DEFAULT_CHOICE.to_string(),
            t(if agent.is_some() {
                "ai-choice-follow-task"
            } else {
                "ai-choice-default"
            }),
        )];
        for (value, profile, model) in self.state.targets(task, off_device) {
            options.push((
                value,
                t_args(
                    "ai-choice-target",
                    &[
                        ("model", arg(model.display_label())),
                        ("provider", arg(provider_name(profile))),
                    ],
                ),
            ));
        }
        if explicit.is_some_and(|assignment| assignment.allow_off_device)
            && !options.iter().any(|(value, _)| *value == selected)
        {
            options.push((selected.clone(), t("ai-choice-allowed-provider")));
        }
        let id = format!("ai.choose-{}", resource_id.replace([':', '/', '.'], "-"));
        body.push(choice_row(
            &t(if agent.is_some() {
                "ai-choose-agent-label"
            } else {
                "ai-choose-label"
            }),
            Box::new(choice_dropdown(
                &resource,
                "choose-model",
                "model",
                options,
                &selected,
                &id,
            )),
        ));
        if let Ok(resolution) = self.state.resolve(task, agent)
            && explicit.is_some()
        {
            if resolution.model.options.fast_mode
                && let Some(binding) = bool_binding(&resource, "set-fast-mode")
            {
                body.push(setting_row(
                    &t("ai-feature-fast-mode"),
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(resolution.options.fast_mode))
                            .with_id(&format!("{id}-fast"))
                            .with_accessible_name(&t("ai-feature-fast-mode"))
                            .on_action(binding),
                    ),
                ));
            }
            if resolution.model.options.reasoning {
                let mut efforts = vec![(DEFAULT_CHOICE.to_string(), t("ai-effort-default"))];
                for effort in agent_provider_config::ReasoningEffort::for_protocol(
                    resolution.profile.protocol,
                ) {
                    efforts.push((effort.wire().to_string(), t(effort.label_key())));
                }
                let chosen = resolution.options.reasoning_effort.map_or_else(
                    || DEFAULT_CHOICE.to_string(),
                    |effort| effort.wire().to_string(),
                );
                body.push(choice_row(
                    &t("ai-feature-reasoning"),
                    Box::new(choice_dropdown(
                        &resource,
                        "set-reasoning-effort",
                        "effort",
                        efforts,
                        &chosen,
                        &format!("{id}-effort"),
                    )),
                ));
            }
        }
        if task.privacy == PrivacyClass::Sensitive
            && agent.is_none()
            && let Some((value, profile, model)) = self
                .state
                .targets(task, true)
                .into_iter()
                .find(|(_, profile, _)| !profile.is_model_manager())
            && !explicit.is_some_and(|assignment| assignment.allow_off_device)
            && let Some(spec) = resource
                .actions
                .iter()
                .find(|spec| spec.id == "allow-off-device")
            && let Ok(binding) = ActionBinding::new(&resource, spec, move |()| {
                serde_json::Map::from_iter([(
                    "model".to_string(),
                    serde_json::Value::String(value.clone()),
                )])
            })
        {
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new(
                            &format!("{id}-allow"),
                            &t_args(
                                "ai-allow-off-device-button",
                                &[
                                    ("model", arg(model.display_label())),
                                    ("provider", arg(provider_name(profile))),
                                ],
                            ),
                            None,
                        )
                        .danger_outline()
                        .on_action(binding),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
        }
        if agent.is_none()
            && let Some(available) = self.state.installable_for(task)
            && self.state.resolve(task, None).is_err()
        {
            body.push(caption(&download_terms(available)));
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        bound_button(
                            tree,
                            Some(resource_id),
                            "set-up-on-device",
                            &format!("{id}-set-up"),
                            &t_args(
                                "ai-set-up-button",
                                &[("model", arg(available.package.name.as_str()))],
                            ),
                        )
                        .primary(),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
        }
    }

    fn tasks_card(&self, tree: &Resource) -> El {
        let mut body = vec![caption(&t("ai-tasks-body"))];
        for task in tasks::REGISTRY {
            let (line, tone) = self.resolution_line(task, None);
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Text::new(&t(task.label_key)).with_scaled_size(14.0),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(privacy_pill(task.privacy)),
            ));
            body.push(caption(&t(task.description_key)));
            body.push(Box::new(
                Row::new()
                    .push(semantic_status_pill(
                        &t(if tone == StatusTone::Success {
                            "ai-status-ready"
                        } else {
                            "ai-status-attention"
                        }),
                        tone,
                    ))
                    .push(hgap(8.0))
                    .push(Box::new(Text::new(&line).with_scaled_size(13.0)))
                    .push(Box::new(Spacer::new())),
            ));
            let resource_id = format!("{TASK_RESOURCE_PREFIX}{}", task.id);
            self.scope_controls(tree, &resource_id, task, None, &mut body);
        }
        card(&t("ai-tasks-title"), rows(body))
    }

    fn agents_card(&self, tree: &Resource) -> El {
        let mut body = vec![caption(&t("ai-agents-body"))];
        if self.state.agents.is_empty() {
            body.push(caption(&t("ai-agents-empty")));
        }
        for agent in &self.state.agents {
            for task in tasks::REGISTRY.iter().filter(|task| task.per_agent) {
                let (line, _) = self.resolution_line(task, Some(agent));
                body.push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Text::new(&t_args(
                                "ai-agent-task",
                                &[
                                    ("agent", arg(agent.as_str())),
                                    ("task", arg(t(task.label_key))),
                                ],
                            ))
                            .with_scaled_size(14.0),
                        ))
                        .push(Box::new(Spacer::new()))
                        .push(semantic_status_pill(
                            &t("ai-agent-pill"),
                            StatusTone::Neutral,
                        )),
                ));
                body.push(caption(&line));
                let resource_id = format!("{AGENT_RESOURCE_PREFIX}{agent}/{}", task.id);
                self.scope_controls(tree, &resource_id, task, Some(agent), &mut body);
            }
        }
        card(&t("ai-agents-title"), rows(body))
    }

    fn health_pill(&self, profile: &ProviderProfile) -> El {
        if signing_in().as_deref() == Some(profile.id.as_str()) {
            return semantic_status_pill(&t("ai-health-signing-in"), StatusTone::Accent);
        }
        if !profile.enabled {
            return semantic_status_pill(&t("ai-health-disabled"), StatusTone::Neutral);
        }
        let Some(health) = self.state.health.get(&profile.id) else {
            return semantic_status_pill(
                &t(if tilo_model_gateway::available() {
                    "ai-health-checking"
                } else {
                    "ai-health-unknown"
                }),
                StatusTone::Neutral,
            );
        };
        let (key, tone) = match (health.credential, health.reachability) {
            (CredentialState::Missing, _) => (
                if matches!(profile.credential, Some(Credential::Account { .. })) {
                    "ai-health-signed-out"
                } else {
                    "ai-health-needs-key"
                },
                StatusTone::Warning,
            ),
            (_, Reachability::Reachable) => ("ai-health-connected", StatusTone::Success),
            (_, Reachability::Unauthorized) => ("ai-health-refused", StatusTone::Warning),
            (_, Reachability::Failed { .. }) => ("ai-health-failed", StatusTone::Warning),
            (_, Reachability::Unreachable) => ("ai-health-unreachable", StatusTone::Warning),
            (_, Reachability::NotChecked) => ("ai-health-unknown", StatusTone::Neutral),
        };
        semantic_status_pill(&t(key), tone)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one provider's endpoint, credential and model controls stay together"
    )]
    fn provider_rows(&self, tree: &Resource, profile: &ProviderProfile, body: &mut Vec<El>) {
        let resource_id = format!("{PROVIDER_RESOURCE_PREFIX}{}", profile.id);
        let Some(resource) = crate::actions::ai_resource(tree, Some(&resource_id)) else {
            return;
        };
        let id = format!("ai.provider-{}", profile.id);
        let buffers = self.buffers_for(profile);
        let is_default = self.state.config.default_profile.as_deref() == Some(profile.id.as_str());
        let mut heading = Row::new()
            .push(Box::new(
                Text::new(profile.display_label()).with_scaled_size(14.0),
            ))
            .push(Box::new(Spacer::new()));
        if is_default {
            heading = heading
                .push(semantic_status_pill(
                    &t("ai-provider-default"),
                    StatusTone::Accent,
                ))
                .push(hgap(8.0));
        }
        body.push(Box::new(heading.push(self.health_pill(profile))));
        body.push(caption(&t_args(
            "ai-provider-kind",
            &[
                ("protocol", arg(t(protocol_key(profile.protocol)))),
                (
                    "location",
                    arg(t(if profile.is_user_local_server() {
                        "ai-location-local-server"
                    } else {
                        location_key(profile.location)
                    })),
                ),
            ],
        )));
        body.push(Box::new(
            TextInput::local()
                .with_id(&format!("{id}-label"))
                .with_external_buffer(Arc::clone(&buffers.label))
                .with_initial_value(&read(&buffers.label))
                .with_placeholder(&t("ai-provider-name-placeholder")),
        ));
        body.push(Box::new(
            TextInput::local()
                .with_id(&format!("{id}-endpoint"))
                .with_external_buffer(Arc::clone(&buffers.endpoint))
                .with_initial_value(&read(&buffers.endpoint))
                .with_placeholder(&t(if profile.protocol == ProviderProtocol::AzureOpenAi {
                    "ai-provider-azure-endpoint-placeholder"
                } else {
                    "ai-provider-endpoint-placeholder"
                })),
        ));
        let details = {
            let (label, endpoint) = (Arc::clone(&buffers.label), Arc::clone(&buffers.endpoint));
            typed_button(
                &resource,
                "set-details",
                &format!("{id}-save"),
                &t("ai-provider-save"),
                move || {
                    serde_json::Map::from_iter([
                        ("label".to_string(), serde_json::Value::String(read(&label))),
                        (
                            "endpoint".to_string(),
                            serde_json::Value::String(read(&endpoint)),
                        ),
                    ])
                },
            )
        };
        let mut actions = Row::new().push(Box::new(details)).push(hgap(8.0));
        if !is_default {
            actions = actions
                .push(Box::new(bound_button(
                    tree,
                    Some(&resource_id),
                    "set-default",
                    &format!("{id}-default"),
                    &t("ai-action-default-provider"),
                )))
                .push(hgap(8.0));
        }
        actions = actions
            .push(Box::new(bound_button(
                tree,
                Some(&resource_id),
                if profile.enabled { "disable" } else { "enable" },
                &format!("{id}-enabled"),
                &t(if profile.enabled {
                    "ai-action-disable-provider"
                } else {
                    "ai-action-enable-provider"
                }),
            )))
            .push(hgap(8.0))
            .push(Box::new(
                bound_button(
                    tree,
                    Some(&resource_id),
                    "remove",
                    &format!("{id}-remove"),
                    &t("ai-action-remove-provider"),
                )
                .danger_outline(),
            ))
            .push(Box::new(Spacer::new()));
        body.push(Box::new(actions));

        // Credential.
        let signed_in = matches!(profile.credential, Some(Credential::Account { .. }));
        if signed_in {
            body.push(value_row(
                &t("ai-provider-credential"),
                &t("ai-provider-signed-in"),
            ));
            body.push(Box::new(
                Row::new()
                    .push(Box::new(bound_button(
                        tree,
                        Some(&resource_id),
                        "sign-out",
                        &format!("{id}-sign-out"),
                        &t("ai-action-sign-out"),
                    )))
                    .push(Box::new(Spacer::new())),
            ));
        } else {
            let key = Arc::clone(&buffers.key);
            body.push(Box::new(
                TextInput::local()
                    .with_id(&format!("{id}-key"))
                    .with_external_buffer(Arc::clone(&buffers.key))
                    .with_secret(true)
                    .with_placeholder(&t("ai-provider-key-placeholder")),
            ));
            let mut credential_row = Row::new()
                .push(Box::new(typed_button(
                    &resource,
                    "set-api-key",
                    &format!("{id}-save-key"),
                    &t("ai-action-set-key"),
                    move || {
                        let taken = std::mem::take(
                            &mut *key
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner),
                        );
                        serde_json::Map::from_iter([(
                            "key".to_string(),
                            serde_json::Value::String(taken),
                        )])
                    },
                )))
                .push(hgap(8.0))
                .push(Box::new(bound_button(
                    tree,
                    Some(&resource_id),
                    "remove-api-key",
                    &format!("{id}-remove-key"),
                    &t("ai-action-remove-key"),
                )));
            if profile.protocol.supports_account_sign_in() {
                let registered = accounts::oauth::ClientSlot::Entra.registration().is_some();
                credential_row = credential_row.push(hgap(8.0)).push(Box::new(
                    bound_button(
                        tree,
                        Some(&resource_id),
                        "sign-in",
                        &format!("{id}-sign-in"),
                        &t("ai-action-sign-in-microsoft"),
                    )
                    .with_enabled(registered && signing_in().is_none()),
                ));
                if !registered {
                    body.push(caption(&t("ai-provider-sign-in-unavailable")));
                }
            }
            body.push(Box::new(credential_row.push(Box::new(Spacer::new()))));
        }

        // Models.
        for model in &profile.models {
            let model_resource_id = format!(
                "{PROVIDER_MODEL_RESOURCE_PREFIX}{}/{}",
                profile.id, model.id
            );
            let Some(model_resource) = crate::actions::ai_resource(tree, Some(&model_resource_id))
            else {
                continue;
            };
            let model_id = format!("{id}-model-{}", model.id.replace(['/', '.', ':'], "-"));
            let context = model.context_window.map_or_else(
                || t("ai-model-context-unknown"),
                |window| t_args("ai-model-context", &[("tokens", f64::from(window).into())]),
            );
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Text::new(model.display_label()).with_scaled_size(13.0),
                    ))
                    .push(hgap(8.0))
                    .push(caption(&context))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        bound_button(
                            tree,
                            Some(&model_resource_id),
                            "remove",
                            &format!("{model_id}-remove"),
                            &t("ai-action-remove-model"),
                        )
                        .ghost(),
                    )),
            ));
            let mut declared = Row::new();
            for (action, key, on) in [
                ("set-tools", "ai-feature-tools", model.tool_calling),
                (
                    "set-vision",
                    ModelCapability::Vision.label_key(),
                    model.declares(ModelCapability::Vision),
                ),
                (
                    "set-trace-review",
                    ModelCapability::TraceReview.label_key(),
                    model.declares(ModelCapability::TraceReview),
                ),
                (
                    "set-fast-mode",
                    "ai-feature-fast-mode",
                    model.options.fast_mode,
                ),
                (
                    "set-reasoning",
                    "ai-feature-reasoning",
                    model.options.reasoning,
                ),
            ] {
                let Some(binding) = bool_binding(&model_resource, action) else {
                    continue;
                };
                declared = declared
                    .push(Box::new(
                        Toggle::from_state(&ui::LocalState::new(on))
                            .with_id(&format!("{model_id}-{action}"))
                            .with_accessible_name(&t(key))
                            .on_action(binding),
                    ))
                    .push(caption(&t(key)))
                    .push(hgap(12.0));
            }
            body.push(Box::new(declared.push(Box::new(Spacer::new()))));
        }
        body.push(Box::new(
            TextInput::local()
                .with_id(&format!("{id}-new-model"))
                .with_external_buffer(Arc::clone(&buffers.model))
                .with_placeholder(&t(if profile.protocol == ProviderProtocol::AzureOpenAi {
                    "ai-provider-deployment-placeholder"
                } else {
                    "ai-provider-model-placeholder"
                })),
        ));
        let (model_buffer, context_buffer) =
            (Arc::clone(&buffers.model), Arc::clone(&buffers.context));
        body.push(Box::new(
            TextInput::local()
                .with_id(&format!("{id}-new-context"))
                .with_external_buffer(Arc::clone(&buffers.context))
                .with_placeholder(&t("ai-provider-context-placeholder")),
        ));
        body.push(Box::new(
            Row::new()
                .push(Box::new(typed_button(
                    &resource,
                    "add-model",
                    &format!("{id}-add-model"),
                    &t(if profile.protocol == ProviderProtocol::AzureOpenAi {
                        "ai-action-add-deployment"
                    } else {
                        "ai-action-add-model"
                    }),
                    move || {
                        let mut arguments = serde_json::Map::from_iter([(
                            "model".to_string(),
                            serde_json::Value::String(read(&model_buffer).trim().to_string()),
                        )]);
                        if let Ok(window) = read(&context_buffer)
                            .trim()
                            .replace(['_', ','], "")
                            .parse::<u32>()
                        {
                            arguments
                                .insert("context_window".into(), serde_json::Value::from(window));
                        }
                        arguments
                    },
                )))
                .push(Box::new(Spacer::new())),
        ));
    }

    fn providers_card(&self, tree: &Resource) -> El {
        let mut body = vec![caption(&t("ai-providers-body"))];
        body.push(Box::new(
            Row::new()
                .push(Box::new(bound_button(
                    tree,
                    None,
                    "add-anthropic",
                    "ai.add-anthropic",
                    &t("ai-action-add-anthropic"),
                )))
                .push(hgap(8.0))
                .push(Box::new(bound_button(
                    tree,
                    None,
                    "add-openai-compatible",
                    "ai.add-openai",
                    &t("ai-action-add-openai"),
                )))
                .push(hgap(8.0))
                .push(Box::new(bound_button(
                    tree,
                    None,
                    "add-azure-openai",
                    "ai.add-azure",
                    &t("ai-action-add-azure"),
                )))
                .push(hgap(8.0))
                .push(Box::new(bound_button(
                    tree,
                    None,
                    "add-private-server",
                    "ai.add-private",
                    &t("ai-action-add-private"),
                )))
                .push(hgap(8.0))
                .push(Box::new(bound_button(
                    tree,
                    None,
                    "add-local-server",
                    "ai.add-local-server",
                    &t("ai-action-add-local-server"),
                )))
                .push(Box::new(Spacer::new())),
        ));
        let hosted: Vec<&ProviderProfile> = self
            .state
            .config
            .profiles
            .iter()
            .filter(|profile| !profile.is_model_manager())
            .collect();
        if hosted.is_empty() {
            body.push(caption(&t("ai-providers-empty")));
        }
        for profile in hosted {
            self.provider_rows(tree, profile, &mut body);
        }
        for issue in self.state.config.validation_errors() {
            body.push(caption(&t_args(
                "ai-provider-issue",
                &[("issue", arg(issue.message))],
            )));
        }
        card(&t("ai-providers-title"), rows(body))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "hardware, accelerator, releases and installed models are one device story"
    )]
    fn device_card(&self, tree: &Resource) -> El {
        let snapshot = match &self.state.snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return card(
                    &t("ai-device-title"),
                    rows(vec![
                        caption(&t("ai-device-unavailable")),
                        caption(&t_args(
                            "ai-device-unavailable-reason",
                            &[("reason", arg(error.as_str()))],
                        )),
                    ]),
                );
            }
        };
        let hardware = &snapshot.hardware;
        let (fit_key, fit_tone) = match hardware.fit {
            HardwareFit::Excellent => ("ai-fit-excellent", StatusTone::Success),
            HardwareFit::Good => ("ai-fit-good", StatusTone::Success),
            HardwareFit::Tight => ("ai-fit-tight", StatusTone::Warning),
            HardwareFit::RestartRequired => ("ai-fit-restart", StatusTone::Warning),
            HardwareFit::Unsupported => ("ai-fit-unsupported", StatusTone::Neutral),
        };
        let mut body = vec![
            value_row(
                &t("ai-device-memory"),
                &t_args(
                    "ai-gib",
                    &[("value", f64::from(hardware.memory_gib).into())],
                ),
            ),
            value_row(&t("ai-device-accelerator"), &hardware.accelerator),
        ];
        if let Some(vram) = hardware.accelerator_memory_gib {
            body.push(value_row(
                &t("ai-device-accelerator-memory"),
                &t_args("ai-gib", &[("value", f64::from(vram).into())]),
            ));
        }
        if let Some(storage) = hardware.free_storage_gib {
            body.push(value_row(
                &t("ai-device-storage"),
                &t_args("ai-gib", &[("value", f64::from(storage).into())]),
            ));
        }
        if let AcceleratorState::RestartRequired { vendor, reason } =
            snapshot.facts.accelerator_state()
        {
            body.push(Box::new(
                Row::new()
                    .push(semantic_status_pill(
                        &t("ai-restart-pill"),
                        StatusTone::Warning,
                    ))
                    .push(hgap(8.0))
                    .push(Box::new(
                        Text::new(&t_args(
                            "ai-restart-required",
                            &[("vendor", arg(vendor.label()))],
                        ))
                        .with_scaled_size(13.0),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
            body.push(caption(&t(match reason {
                RestartReason::DriverLibraryMismatch => "ai-restart-mismatch",
                RestartReason::DriverStaged => "ai-restart-staged",
            })));
        } else {
            body.push(caption(&hardware.reason));
        }
        if let Some(setup) = &snapshot.accelerator_setup {
            let (key, detail) = match &setup.status {
                AcceleratorSetupState::Available => (
                    "ai-accelerator-available",
                    t("ai-accelerator-available-detail"),
                ),
                AcceleratorSetupState::Queued => {
                    ("ai-accelerator-queued", t("ai-accelerator-queued-detail"))
                }
                AcceleratorSetupState::Staging => {
                    ("ai-accelerator-staging", t("ai-accelerator-staging-detail"))
                }
                AcceleratorSetupState::RestartRequired => {
                    ("ai-accelerator-restart", t("ai-accelerator-restart-detail"))
                }
                AcceleratorSetupState::Active => {
                    ("ai-accelerator-active", t("ai-accelerator-active-detail"))
                }
                AcceleratorSetupState::Error { message } => {
                    ("ai-accelerator-error", message.clone())
                }
            };
            body.push(value_row(&t("ai-accelerator-title"), &t(key)));
            if setup.status.is_transient() {
                body.push(Box::new(ProgressBar::indeterminate().with_label(t(key))));
            }
            body.push(caption(&detail));
            if matches!(
                setup.status,
                AcceleratorSetupState::Available | AcceleratorSetupState::Error { .. }
            ) {
                body.push(Box::new(
                    Row::new()
                        .push(Box::new(bound_button(
                            tree,
                            None,
                            "stage-accelerator",
                            "ai.stage-accelerator",
                            &t("ai-action-stage-accelerator"),
                        )))
                        .push(Box::new(Spacer::new())),
                ));
            }
        }

        let available: Vec<&AvailableModel> = snapshot
            .available
            .iter()
            .filter(|available| {
                !snapshot
                    .models
                    .iter()
                    .any(|model| model.package.id == available.package.id)
            })
            .collect();
        body.push(Box::new(
            Text::new(&t("ai-releases-title")).with_scaled_size(14.0),
        ));
        if available.is_empty() {
            body.push(caption(&t(if snapshot.available.is_empty() {
                "ai-releases-none-fit"
            } else {
                "ai-releases-all-installed"
            })));
        }
        for release in available {
            let resource_id = format!("{RELEASE_RESOURCE_PREFIX}{}", release.package.id);
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Text::new(&release.package.name).with_scaled_size(13.0),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(semantic_status_pill(
                        &speed_label(release),
                        if release.speed.class == SpeedClass::Interactive {
                            StatusTone::Success
                        } else {
                            StatusTone::Warning
                        },
                    )),
            ));
            body.push(caption(&release.package.description));
            body.push(caption(&capabilities_text(&release.package)));
            body.push(caption(&download_terms(release)));
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        bound_button(
                            tree,
                            Some(&resource_id),
                            "install",
                            &format!(
                                "ai.install-{}",
                                release.package.id.replace(['/', '.', ':'], "-")
                            ),
                            &t_args(
                                "ai-action-download",
                                &[("model", arg(release.package.name.as_str()))],
                            ),
                        )
                        .primary(),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
        }

        body.push(Box::new(
            Text::new(&t("ai-installed-title")).with_scaled_size(14.0),
        ));
        if snapshot.models.is_empty() {
            body.push(caption(&t("ai-installed-empty")));
        }
        for model in &snapshot.models {
            Self::installed_rows(tree, model, &mut body);
        }
        card_with_action(
            &t("ai-device-title"),
            semantic_status_pill(&t(fit_key), fit_tone),
            rows(body),
        )
    }

    fn installed_rows(tree: &Resource, model: &InstalledLocalModel, body: &mut Vec<El>) {
        let resource_id = format!("{INSTALLED_RESOURCE_PREFIX}{}", model.package.id);
        let id = format!(
            "ai.local-{}",
            model.package.id.replace(['/', '.', ':'], "-")
        );
        let running = matches!(model.phase, LocalModelPhase::Ready { running: true, .. });
        body.push(Box::new(
            Row::new()
                .push(Box::new(
                    Text::new(&model.package.name).with_scaled_size(13.0),
                ))
                .push(Box::new(Spacer::new()))
                .push(semantic_status_pill(
                    &phase_status(&model.phase),
                    match model.phase {
                        LocalModelPhase::Ready { running: true, .. } => StatusTone::Success,
                        LocalModelPhase::Error { .. } => StatusTone::Warning,
                        _ => StatusTone::Neutral,
                    },
                )),
        ));
        if model.phase.is_transient() {
            let label = t_args(
                "ai-progress-label",
                &[("model", arg(model.package.name.as_str()))],
            );
            body.push(Box::new(match phase_progress(&model.phase) {
                Some(progress) => ProgressBar::new(progress).with_label(label),
                None => ProgressBar::indeterminate().with_label(label),
            }));
        }
        body.push(caption(&phase_detail(&model.phase)));
        body.push(caption(&capabilities_text(&model.package)));
        if running && model.package.accelerator_memory_mib > 0 {
            body.push(value_row(
                &t("ai-local-vram"),
                &t_args(
                    "ai-gib",
                    &[(
                        "value",
                        (model.package.accelerator_memory_mib as f64 / 1024.0).into(),
                    )],
                ),
            ));
        }
        body.push(value_row(
            &t("ai-local-storage"),
            &t_args(
                "ai-gib-text",
                &[("value", arg(gib(model.package.installed_bytes)))],
            ),
        ));
        body.push(value_row(&t("ai-local-license"), &model.package.license));
        let mut actions = Row::new();
        for (action, key) in [
            ("start", "ai-action-start"),
            ("stop", "ai-action-stop"),
            ("retry", "ai-action-retry"),
        ] {
            if let Some(binding) = crate::actions::ai_binding(tree, Some(&resource_id), action) {
                actions = actions
                    .push(Box::new(
                        Button::new(&format!("{id}-{action}"), &t(key), None)
                            .fit_content(40.0)
                            .on_action(binding),
                    ))
                    .push(hgap(8.0));
            }
        }
        if let Some(binding) = crate::actions::ai_binding(tree, Some(&resource_id), "remove") {
            actions = actions.push(Box::new(
                Button::new(&format!("{id}-remove"), &t("ai-action-remove-local"), None)
                    .fit_content(40.0)
                    .danger_outline()
                    .on_action(binding),
            ));
        }
        body.push(Box::new(actions.push(Box::new(Spacer::new()))));
    }
}

impl Default for AiPane {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AiPane {
    fn drop(&mut self) {
        if let Ok(buffers) = self.buffers.lock() {
            for buffer in buffers.values() {
                if let Ok(mut key) = buffer.key.lock() {
                    key.zeroize();
                }
            }
        }
    }
}

fn provider_name(profile: &ProviderProfile) -> String {
    if profile.is_model_manager() {
        t("ai-this-device")
    } else {
        profile.display_label().to_string()
    }
}

fn protocol_key(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::Anthropic => "ai-protocol-anthropic",
        ProviderProtocol::OpenAiCompatible => "ai-protocol-openai",
        ProviderProtocol::AzureOpenAi => "ai-protocol-azure",
    }
}

fn location_key(location: EndpointLocation) -> &'static str {
    match location {
        EndpointLocation::Hosted => "ai-location-hosted",
        EndpointLocation::ThisDevice => "ai-location-device",
        EndpointLocation::PrivateNetwork => "ai-location-private",
    }
}

fn privacy_pill(privacy: PrivacyClass) -> El {
    let (key, tone) = match privacy {
        PrivacyClass::Open => ("ai-privacy-open", StatusTone::Neutral),
        PrivacyClass::Sensitive => ("ai-privacy-sensitive", StatusTone::Accent),
        PrivacyClass::DeviceOnly => ("ai-privacy-device-only", StatusTone::Success),
    };
    semantic_status_pill(&t(key), tone)
}

fn capabilities_text(package: &LocalModelPackage) -> String {
    let mut good_for: Vec<String> = package
        .capabilities
        .iter()
        .map(|capability| t(capability.label_key()))
        .collect();
    if good_for.is_empty() {
        good_for.push(t("ai-capability-text"));
    }
    t_args("ai-good-for", &[("list", arg(good_for.join(", ")))])
}

fn resolve_error_text(error: &ResolveError) -> String {
    match error {
        ResolveError::RestartRequired { vendor } => {
            t_args("ai-restart-required", &[("vendor", arg(vendor.label()))])
        }
        ResolveError::NoModel { privacy } => t(match privacy {
            PrivacyClass::Open => "ai-error-no-model",
            PrivacyClass::Sensitive | PrivacyClass::DeviceOnly => "ai-error-no-local-model",
        }),
        ResolveError::OffDevice { .. } => t("ai-error-off-device"),
        ResolveError::ProfileUnavailable { .. } => t("ai-error-provider-unavailable"),
        ResolveError::ModelUnavailable { model_id } => t_args(
            "ai-error-model-unavailable",
            &[("model", arg(model_id.as_str()))],
        ),
        ResolveError::Unmet { .. } => t("ai-error-unmet"),
        ResolveError::Unpinned { .. } => t("ai-error-unpinned"),
        ResolveError::Options { .. } => t("ai-error-options"),
        ResolveError::UnknownTask | ResolveError::NotConsumer => t("ai-error-no-model"),
    }
}

/// A button bound to the nullary action `action` of `resource_id`; disabled
/// when the tree does not export it now.
fn bound_button(
    tree: &Resource,
    resource_id: Option<&str>,
    action: &str,
    id: &str,
    label: &str,
) -> Button {
    let button = Button::new(id, label, None).fit_content(40.0);
    match crate::actions::ai_binding(tree, resource_id, action) {
        Some(binding) => button.on_action(binding),
        None => button.with_enabled(false),
    }
}

/// A button bound to a typed action whose arguments come from `arguments`
/// when it is pressed.
fn typed_button(
    resource: &Resource,
    action: &str,
    id: &str,
    label: &str,
    arguments: impl Fn() -> serde_json::Map<String, serde_json::Value> + Send + Sync + 'static,
) -> Button {
    let button = Button::new(id, label, None).fit_content(40.0);
    let binding = resource
        .actions
        .iter()
        .find(|spec| spec.id == action)
        .and_then(|spec| ActionBinding::new(resource, spec, move |()| arguments()).ok());
    match binding {
        Some(binding) => button.on_action(binding),
        None => button.with_enabled(false),
    }
}

fn bool_binding(resource: &Resource, action: &str) -> Option<ActionBinding<bool>> {
    let spec = resource.actions.iter().find(|spec| spec.id == action)?;
    ActionBinding::new(resource, spec, |enabled: bool| {
        serde_json::Map::from_iter([("enabled".to_string(), serde_json::Value::Bool(enabled))])
    })
    .ok()
}

/// A dropdown over `(value, label)` options bound to the typed action
/// `action`, whose one parameter `param` receives the chosen value.
fn choice_dropdown(
    resource: &Resource,
    action: &str,
    param: &'static str,
    options: Vec<(String, String)>,
    selected: &str,
    id: &str,
) -> Dropdown {
    let values: Vec<String> = options.iter().map(|(value, _)| value.clone()).collect();
    let mut dropdown =
        Dropdown::from_state(values, &ui::LocalState::new(selected.to_string())).with_id(id);
    dropdown.set_value_options(options);
    let binding = resource
        .actions
        .iter()
        .find(|spec| spec.id == action)
        .and_then(|spec| {
            ActionBinding::new(resource, spec, move |choice: String| {
                serde_json::Map::from_iter([(param.to_string(), serde_json::Value::String(choice))])
            })
            .ok()
        });
    match binding {
        Some(binding) => dropdown.on_action(binding),
        None => dropdown.with_enabled(false),
    }
}

impl Widget for AiPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(crate::actions::LocalPaneCommand::AgentModel(operation)) =
            self.command_state.get()
        {
            self.command_state.set(None);
            self.run_model_operation(&operation);
        }
        if let Some(composed) = &mut self.composed {
            composed.sync(state, root);
        }
    }

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        let revision = REVISION.load(Ordering::Relaxed);
        if revision != self.revision {
            self.revision = revision;
            if let Some(notice) = NOTICE.lock().ok().and_then(|mut notice| notice.take()) {
                self.notice = Some(notice);
            }
            if let Ok(probed) = self.probed.lock() {
                for (id, health) in probed.iter() {
                    self.state.health.insert(id.clone(), *health);
                }
                self.probing = probed.len()
                    < self
                        .state
                        .config
                        .profiles
                        .iter()
                        .filter(|profile| !profile.is_model_manager())
                        .count()
                    && self.probing;
            }
            self.refresh(false);
            return true;
        }
        let busy = self.is_busy();
        if busy && self.last_poll.elapsed() >= POLL_INTERVAL {
            self.refresh(true);
            return true;
        }
        busy
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&child) = children.first()
        {
            composed.apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(composed) = &mut self.composed {
            composed.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The Widget event contract forwards input, CRDT state, focus/handled outputs, and layout to the composed settings pane."
    )]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&child) = children.first()
        {
            composed.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|composed| composed.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|composed| composed.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "AiPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
            || self
                .composed
                .as_mut()
                .is_some_and(|composed| composed.take_structure_dirty())
    }
}

/// A deterministic hub for `--shots`: a device with an 8 GiB card, an Azure
/// deployment assigned to one agent in fast mode, and signed releases to
/// download. Sample data lives only here.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the whole deterministic fixture reads top to bottom in one place"
)]
pub fn demo_state(restart_pending: bool) -> AiState {
    use agent_provider_config::assign::{Assignment, ModelAssignments, ModelTarget};
    use agent_provider_config::local::{
        Accelerator, AcceleratorVendor, ExpectedSpeed, HardwareSnapshot, LocalEndpoint, RuntimeKind,
    };
    use agent_provider_config::manager::HardwareRecommendation;
    use agent_provider_config::{
        ModelDescriptor, ModelOptionSupport, ModelOptions, ProviderConfig,
    };

    let package =
        |id: &str, name: &str, description: &str, capabilities: &[ModelCapability], gib: u64| {
            LocalModelPackage {
                id: id.into(),
                name: name.into(),
                description: description.into(),
                source: format!("https://huggingface.co/{id}"),
                revision: "5f1b0d6c2a9e4b7f8c3d1e0a9b8c7d6e5f4a3b2c".into(),
                license: "Apache-2.0".into(),
                license_url: format!("https://huggingface.co/{id}/blob/main/LICENSE"),
                download_bytes: gib * 1_073_741_824,
                installed_bytes: gib * 1_073_741_824 + 300_000_000,
                capabilities: capabilities.iter().copied().collect(),
                context_window: 32_768,
                accelerator_memory_mib: gib * 1024 + 1024,
            }
        };
    let reviewer = package(
        "qwen/qwen3-4b-awq",
        "Qwen3 4B",
        "A compact general model for private review and short tasks.",
        &[ModelCapability::TraceReview],
        3,
    );
    let vision = package(
        "smolvlm2-500m",
        "SmolVLM2 500M",
        "Describes photos and reads the text in them for local search.",
        &[ModelCapability::Vision],
        1,
    );
    let faces = package(
        "tilo/face-cluster",
        "Face clusters",
        "Groups photos by the faces in them, on this device only.",
        &[ModelCapability::FaceRecognition],
        1,
    );
    let accelerators = if restart_pending {
        vec![Accelerator::RestartRequired {
            vendor: AcceleratorVendor::Nvidia,
            reason: RestartReason::DriverStaged,
        }]
    } else {
        vec![Accelerator::Nvidia {
            vram_mib: 8188,
            driver: "580.82".into(),
        }]
    };
    let hardware = HardwareSnapshot {
        cpu_threads: 16,
        memory_mib: 32 * 1024,
        available_memory_mib: 22 * 1024,
        free_storage_mib: 180 * 1024,
        accelerators,
    };
    let running = !restart_pending;
    let snapshot = LocalModelSnapshot {
        hardware: HardwareRecommendation {
            fit: if restart_pending {
                HardwareFit::RestartRequired
            } else {
                HardwareFit::Good
            },
            memory_gib: 32,
            accelerator: if restart_pending {
                "NVIDIA".into()
            } else {
                "NVIDIA CUDA".into()
            },
            accelerator_memory_gib: (!restart_pending).then_some(8),
            free_storage_gib: Some(180),
            reason: "A signed CUDA runtime was selected from locally probed driver and VRAM facts."
                .into(),
        },
        facts: hardware,
        recommendation: Some(reviewer.clone()),
        available: vec![
            AvailableModel {
                package: vision.clone(),
                runtime: RuntimeKind::NvidiaCuda,
                speed: ExpectedSpeed {
                    tokens_per_second: 180,
                    class: SpeedClass::Interactive,
                },
            },
            AvailableModel {
                package: faces,
                runtime: RuntimeKind::NvidiaCuda,
                speed: ExpectedSpeed {
                    tokens_per_second: 140,
                    class: SpeedClass::Interactive,
                },
            },
            AvailableModel {
                package: reviewer.clone(),
                runtime: RuntimeKind::NvidiaCuda,
                speed: ExpectedSpeed {
                    tokens_per_second: 96,
                    class: SpeedClass::Interactive,
                },
            },
        ],
        models: vec![
            InstalledLocalModel {
                package: reviewer,
                phase: LocalModelPhase::Ready {
                    running,
                    endpoint: running.then(|| LocalEndpoint {
                        socket: "/run/user/1000/tilo/models/qwen3/api.sock".into(),
                        model_id: "qwen/qwen3-4b-awq".into(),
                    }),
                },
            },
            InstalledLocalModel {
                package: vision,
                phase: LocalModelPhase::Downloading {
                    downloaded_bytes: 700_000_000,
                    total_bytes: 1_073_741_824,
                },
            },
        ],
        accelerator_setup: None,
    };
    let mut azure = ProviderProfile::hosted(ProviderProtocol::AzureOpenAi);
    azure.id = "8e7f6d5c-work-azure".into();
    azure.label = "Work Azure".into();
    azure.endpoint = "https://contoso-ai.openai.azure.com/openai/v1".into();
    azure.credential = Some(Credential::Account {
        account_id: "entra-work".into(),
    });
    azure.models = vec![ModelDescriptor {
        context_window: Some(400_000),
        options: ModelOptionSupport {
            fast_mode: true,
            reasoning: true,
        },
        ..ModelDescriptor::new("gpt-5.6-sol")
    }];
    azure.default_model = Some("gpt-5.6-sol".into());
    let mut anthropic = ProviderProfile::hosted(ProviderProtocol::Anthropic);
    anthropic.id = "2b3c4d5e-anthropic".into();
    anthropic.models = vec![ModelDescriptor {
        context_window: Some(1_000_000),
        options: ModelOptionSupport {
            fast_mode: true,
            reasoning: true,
        },
        ..ModelDescriptor::new("claude-opus-5")
    }];
    anthropic.default_model = Some("claude-opus-5".into());
    let mut ollama = ProviderProfile::new(
        ProviderProtocol::OpenAiCompatible,
        EndpointLocation::ThisDevice,
    );
    ollama.id = "5a6b7c8d-ollama".into();
    ollama.label = "Ollama".into();
    ollama.models = vec![ModelDescriptor {
        context_window: Some(32_768),
        ..ModelDescriptor::new("llama3.3")
    }];
    ollama.default_model = Some("llama3.3".into());
    let mut config = ProviderConfig {
        profiles: vec![anthropic.clone(), azure.clone(), ollama],
        default_profile: Some(anthropic.id.clone()),
    };
    if let Some(profile) = snapshot.runtime().profile {
        config.profiles.push(profile);
    }
    let mut assignments = ModelAssignments {
        preset: Some(Preset::Balanced),
        ..ModelAssignments::default()
    };
    assignments.set_agent(
        "Claude",
        tasks::AGENT_PLANNING.id,
        Assignment {
            target: ModelTarget::Profile {
                profile_id: azure.id.clone(),
                model_id: "gpt-5.6-sol".into(),
            },
            options: ModelOptions {
                fast_mode: true,
                reasoning_effort: None,
            },
            allow_off_device: false,
        },
    );
    let mut health = BTreeMap::new();
    health.insert(
        anthropic.id,
        ProviderHealth {
            credential: CredentialState::Present,
            reachability: Reachability::Reachable,
        },
    );
    health.insert(
        azure.id,
        ProviderHealth {
            credential: CredentialState::Present,
            reachability: Reachability::Reachable,
        },
    );
    AiState {
        config,
        assignments,
        snapshot: Ok(snapshot),
        health,
        agents: vec!["Claude".into(), "Scribe".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_accessibility(widget: &dyn Widget, nodes: &mut Vec<ui::a11y::AccessNode>) {
        if let Some(node) = widget.accessibility() {
            nodes.push(node);
        }
        for child in widget.children() {
            collect_accessibility(child, nodes);
        }
    }

    fn names(state: AiState) -> Vec<String> {
        let pane = AiPane::for_state(state);
        let composition = pane.build_composition();
        let mut nodes = Vec::new();
        collect_accessibility(composition.as_ref(), &mut nodes);
        nodes.into_iter().filter_map(|node| node.name).collect()
    }

    /// The hub shows providers, tasks, agents, presets and this device; a card waiting for a restart
    /// says so instead of offering the CPU; and an unavailable manager never fabricates a release.
    #[test]
    fn the_hub_shows_what_is_there_and_never_fabricates_a_release() {
        // Case `the_hub_shows_providers_tasks_agents_presets_and_this_device`.
        {
            let names = names(demo_state(false));
            for expected in [
                "Balanced",
                "Feedback review",
                "Work Azure",
                "Qwen3 4B",
                "Claude · Agent planning",
            ] {
                assert!(
                    names.iter().any(|name| name.contains(expected)),
                    "{expected} missing from {names:?}"
                );
            }
        }

        // Case `a_card_waiting_for_a_restart_says_so_instead_of_offering_the_cpu`.
        {
            let names = names(demo_state(true));
            assert!(
                names
                    .iter()
                    .any(|name| name.contains("Restart to finish setting up")),
                "{names:?}"
            );
        }

        // Case `an_unavailable_manager_never_fabricates_a_release`.
        {
            let mut state = demo_state(false);
            state.snapshot = Err("model service is unavailable".into());
            let names = names(state);
            assert!(names.iter().all(|name| !name.contains("Download SmolVLM2")));
        }
    }

    /// A control that records each command the pane sends to the model
    /// manager and accepts them all.
    #[derive(Default)]
    struct RecordingControl {
        snapshot: Mutex<Option<LocalModelSnapshot>>,
        calls: Mutex<Vec<String>>,
    }

    impl RecordingControl {
        fn record(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl LocalModelControl for RecordingControl {
        fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
            self.snapshot
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| "no snapshot".into())
        }
        fn install(&self, model: &LocalModelPackage) -> Result<(), String> {
            self.record(format!("install:{}", model.id));
            Ok(())
        }
        fn start(&self, id: &str) -> Result<(), String> {
            self.record(format!("start:{id}"));
            Ok(())
        }
        fn stop(&self, id: &str) -> Result<(), String> {
            self.record(format!("stop:{id}"));
            Ok(())
        }
        fn remove(&self, id: &str) -> Result<(), String> {
            self.record(format!("remove:{id}"));
            Ok(())
        }
        fn stage_accelerator(&self) -> Result<(), String> {
            self.record("stage-accelerator".into());
            Ok(())
        }
    }

    /// A demo state whose installed models stand in each lifecycle phase:
    /// stopped, running, downloading and failed.
    fn lifecycle_state() -> AiState {
        let mut state = demo_state(false);
        let snapshot = state.snapshot.as_mut().unwrap();
        let package = snapshot.models[0].package.clone();
        let with_id = |id: &str| LocalModelPackage {
            id: id.into(),
            ..package.clone()
        };
        snapshot.models = vec![
            InstalledLocalModel {
                package: with_id("m/stopped"),
                phase: LocalModelPhase::Ready {
                    running: false,
                    endpoint: None,
                },
            },
            InstalledLocalModel {
                package: with_id("m/running"),
                phase: LocalModelPhase::Ready {
                    running: true,
                    endpoint: None,
                },
            },
            InstalledLocalModel {
                package: with_id("m/downloading"),
                phase: LocalModelPhase::Downloading {
                    downloaded_bytes: 1,
                    total_bytes: 2,
                },
            },
            InstalledLocalModel {
                package: with_id("m/failed"),
                phase: LocalModelPhase::Error {
                    message: "bad digest".into(),
                },
            },
        ];
        state
    }

    fn action_ids(hub: &Resource, resource: &str) -> Vec<String> {
        crate::actions::ai_resource(hub, Some(resource))
            .unwrap_or_else(|| panic!("{resource} is in the hub"))
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect()
    }

    /// Each installed model offers only the controls its phase allows, and
    /// removing one is destructive, confirmed and never runs until the
    /// confirmed command reaches the pane.
    #[test]
    fn local_models_offer_only_safe_controls_and_confirm_removal() {
        let hub = crate::actions::ai_section_for(&lifecycle_state());
        let installed = |id: &str| format!("{INSTALLED_RESOURCE_PREFIX}{id}");
        assert_eq!(
            action_ids(&hub, &installed("m/stopped")),
            ["start", "remove"]
        );
        assert_eq!(
            action_ids(&hub, &installed("m/running")),
            ["stop", "remove"]
        );
        assert_eq!(
            action_ids(&hub, &installed("m/failed")),
            ["retry", "remove"]
        );
        assert!(
            action_ids(&hub, &installed("m/downloading")).is_empty(),
            "a model still downloading can be neither started nor removed"
        );
        for id in ["m/stopped", "m/running", "m/failed"] {
            let resource = crate::actions::ai_resource(&hub, Some(&installed(id))).unwrap();
            let remove = resource
                .actions
                .iter()
                .find(|action| action.id == "remove")
                .unwrap();
            assert_eq!(
                remove.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm,
                "{id}"
            );
            assert_eq!(
                remove.contract.risk,
                tilo_actions::RiskLevel::Destructive,
                "{id}"
            );
            let start_or_stop = resource
                .actions
                .iter()
                .find(|action| action.id != "remove")
                .unwrap();
            assert_ne!(
                start_or_stop.contract.risk,
                tilo_actions::RiskLevel::Destructive,
                "{id}: lifecycle controls never destroy anything"
            );
        }

        // Nothing reaches the manager until the confirmed command arrives,
        // and then exactly that one removal does.
        let control = Arc::new(RecordingControl::default());
        let mut pane = AiPane::from_parts(control.clone(), lifecycle_state(), true);
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        assert!(control.calls().is_empty());
        let commands = ui::LocalState::new(None);
        let mut pane = pane.with_command_state(&commands);
        commands.set(Some(crate::actions::LocalPaneCommand::AgentModel(
            crate::actions::AgentModelOperation::Remove {
                id: "m/stopped".into(),
            },
        )));
        pane.sync(&mut doc, &automerge::ROOT);
        assert_eq!(control.calls(), ["remove:m/stopped"]);
    }

    /// A finished command reports a terminal receipt only to the operation
    /// that is running; another command's completion never closes it.
    #[test]
    fn a_model_command_reports_a_receipt_only_for_its_own_operation() {
        let token = |operation: &crate::actions::AgentModelOperation| {
            crate::actions::local_operation_token_for(
                crate::actions::AGENT_MODEL_OPERATION_TYPE,
                operation,
            )
        };
        let start = crate::actions::AgentModelOperation::Start {
            id: "m/stopped".into(),
        };
        let stop_other = crate::actions::AgentModelOperation::Stop {
            id: "m/running".into(),
        };
        let control = Arc::new(RecordingControl::default());
        let status = ui::LocalState::new(crate::actions::LocalOperationStatus::Idle);
        let commands = ui::LocalState::new(None);
        let mut pane = AiPane::from_parts(control.clone(), lifecycle_state(), true)
            .with_operation_status(&status)
            .with_command_state(&commands);
        let mut doc = AutoCommit::new();

        status.set(crate::actions::LocalOperationStatus::Running {
            token: token(&start),
        });
        commands.set(Some(crate::actions::LocalPaneCommand::AgentModel(
            start.clone(),
        )));
        pane.sync(&mut doc, &automerge::ROOT);
        assert_eq!(control.calls(), ["start:m/stopped"]);
        assert!(matches!(
            status.get(),
            crate::actions::LocalOperationStatus::Succeeded { token: done, .. } if done == token(&start)
        ));

        // A different operation is running: this command's completion leaves
        // its receipt alone.
        status.set(crate::actions::LocalOperationStatus::Running {
            token: token(&stop_other),
        });
        commands.set(Some(crate::actions::LocalPaneCommand::AgentModel(start)));
        pane.sync(&mut doc, &automerge::ROOT);
        assert!(matches!(
            status.get(),
            crate::actions::LocalOperationStatus::Running { token: running } if running == token(&stop_other)
        ));
    }

    /// Setting up the graphics driver is its own confirmed, privileged action
    /// on the pane, never a control on a model, and it touches nothing else.
    #[test]
    fn accelerator_setup_is_separate_from_the_model_lifecycle() {
        let mut state = lifecycle_state();
        state.snapshot.as_mut().unwrap().accelerator_setup =
            Some(agent_provider_config::manager::AcceleratorSetup {
                boot_pack_id: "gpu-stack".into(),
                status: AcceleratorSetupState::Available,
            });
        let hub = crate::actions::ai_section_for(&state);
        let driver_setup = hub
            .actions
            .iter()
            .find(|action| action.id == "stage-accelerator")
            .expect("the pane offers the driver setup");
        assert_eq!(
            driver_setup.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert_eq!(
            driver_setup.contract.risk,
            tilo_actions::RiskLevel::Privileged
        );
        for id in ["m/stopped", "m/running", "m/failed", "m/downloading"] {
            assert!(
                !action_ids(&hub, &format!("{INSTALLED_RESOURCE_PREFIX}{id}"))
                    .iter()
                    .any(|action| action == "stage-accelerator"),
                "{id}"
            );
        }

        let control = Arc::new(RecordingControl::default());
        let commands = ui::LocalState::new(None);
        let mut pane =
            AiPane::from_parts(control.clone(), state, true).with_command_state(&commands);
        commands.set(Some(crate::actions::LocalPaneCommand::AgentModel(
            crate::actions::AgentModelOperation::StageAccelerator,
        )));
        pane.sync(&mut AutoCommit::new(), &automerge::ROOT);
        assert_eq!(control.calls(), ["stage-accelerator"]);
    }

    /// Download terms name size, license and speed, and progress is bounded and machine readable.
    #[test]
    fn downloads_state_their_terms_and_report_bounded_progress() {
        // Case `download_terms_name_size_license_and_speed`.
        {
            let state = demo_state(false);
            let snapshot = state.snapshot.as_ref().unwrap();
            let terms = download_terms(&snapshot.available[0]);
            assert!(terms.contains("Apache-2.0"), "{terms}");
            assert!(terms.contains("1.0"), "{terms}");
        }

        // Case `progress_is_bounded_and_machine_readable`.
        {
            let phase = LocalModelPhase::Downloading {
                downloaded_bytes: 3,
                total_bytes: 4,
            };
            assert_eq!(phase_progress(&phase), Some(0.75));
            let presentation = phase_presentation(&phase);
            assert!(presentation.is_valid());
        }
    }
}
