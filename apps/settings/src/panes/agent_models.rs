//! Agent Models — device-local model lifecycle and provider profiles.
//!
//! Tilo owns the user-facing lifecycle, while a small adapter speaks to the
//! privileged model manager that pins Hugging Face snapshots and supervises
//! vLLM. Settings never downloads model bytes or starts a server itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
};
use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, ProgressBar, Row, Spacer, Text, TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use zeroize::Zeroize;

use crate::widgets::{El, caption, card, card_with_action, hgap, rows, value_row};

const SETUP_KEY: &str = "agent_models.local.setup";
pub(crate) const CONFIRM_KEY: &str = "agent_models.local.confirm";
const CANCEL_KEY: &str = "agent_models.local.cancel";
pub(crate) const ACCELERATOR_SETUP_KEY: &str = "agent_models.accelerator.setup";
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

#[derive(Clone)]
struct HostedFieldBuffers {
    label: Arc<Mutex<String>>,
    endpoint: Arc<Mutex<String>>,
    model: Arc<Mutex<String>>,
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
    fn label(self) -> &'static str {
        match self {
            Self::Excellent => "Excellent fit",
            Self::Good => "Good fit",
            Self::Tight => "Will run, with limits",
            Self::Unsupported => "Not enough memory",
        }
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

    fn status(&self) -> &'static str {
        match self {
            Self::Downloading { .. } => "Downloading",
            Self::Installing { .. } => "Installing",
            Self::Ready { running: true, .. } => "Running",
            Self::Ready { running: false, .. } => "Ready",
            Self::Error { .. } => "Needs attention",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledLocalModel {
    pub package: LocalModelPackage,
    pub phase: LocalModelPhase,
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
    #[serde(default)]
    pub accelerator_setup: Option<AcceleratorSetup>,
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

/// Read the manager's current, signed inventory for the resource tree and
/// effect-time validation.  This performs no mutation and deliberately keeps
/// manager implementation details out of `actions.rs`.
///
/// # Errors
///
/// Returns the local manager's unavailable or malformed-snapshot error.
pub fn live_snapshot() -> Result<LocalModelSnapshot, String> {
    LocalModelControl::snapshot(&ModelManagerCli)
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
        (
            HardwareFit::Excellent,
            "Your memory can keep a larger local model responsive while other apps are open.",
        )
    } else if memory_gib >= 16 {
        (
            HardwareFit::Good,
            "Balanced for everyday tool use and this device's available system memory.",
        )
    } else if memory_gib >= 8 {
        (
            HardwareFit::Tight,
            "A compact model leaves room for the desktop and workspace apps.",
        )
    } else {
        (
            HardwareFit::Unsupported,
            "This device does not report enough memory for reliable local inference.",
        )
    };
    LocalModelSnapshot {
        hardware: HardwareRecommendation {
            fit,
            memory_gib,
            accelerator: "Checked by the model service at install".into(),
            accelerator_memory_gib: None,
            free_storage_gib: None,
            reason: reason.into(),
        },
        recommendation: None,
        models: Vec::new(),
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

fn action_label(action: ModelAction) -> &'static str {
    match action {
        ModelAction::Start => "Start",
        ModelAction::Stop => "Stop",
        ModelAction::Remove => "Remove…",
        ModelAction::Retry => "Retry install",
    }
}

fn action_slug(action: ModelAction) -> &'static str {
    match action {
        ModelAction::Start => "start",
        ModelAction::Stop => "stop",
        ModelAction::Remove => "remove",
        ModelAction::Retry => "retry",
    }
}

pub(crate) fn model_key(id: &str, action: ModelAction) -> String {
    let token = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("agent_models.local.{token}.{}", action_slug(action))
}

fn byte_size(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / 1_073_741_824.0)
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

fn provider_action_button(
    resources: &tilo_actions::Resource,
    id: &str,
    label: &str,
    profile_id: Option<&str>,
    action_id: &str,
) -> Button {
    let button = Button::new(id, label, None);
    match crate::actions::agent_provider_binding(resources, profile_id, action_id) {
        Some(binding) => button.on_action(binding),
        None => button.with_enabled(false),
    }
}

fn sync_local_profile(models: &[InstalledLocalModel]) {
    let mut config = ProviderConfig::load();
    let existing = config
        .profiles
        .iter()
        .position(ProviderProfile::is_local_vllm_candidate);
    let Some(profile) = synced_local_profile(
        existing.and_then(|index| config.profiles.get(index).cloned()),
        models,
    ) else {
        return;
    };
    let changed = existing.and_then(|index| config.profiles.get(index)) != Some(&profile);
    if changed {
        config.upsert(profile);
        let _ = config.save();
    }
}

fn synced_local_profile(
    existing: Option<ProviderProfile>,
    models: &[InstalledLocalModel],
) -> Option<ProviderProfile> {
    let ready = models
        .iter()
        .filter(|model| matches!(model.phase, LocalModelPhase::Ready { .. }))
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
            context_window: None,
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "the independent lifecycle, confirmation, and rebuild flags model concurrent manager state"
)]
pub struct AgentModelsPane {
    control: Arc<dyn LocalModelControl>,
    snapshot: LocalModelSnapshot,
    manager_available: bool,
    sync_provider_profile: bool,
    confirmation: Option<String>,
    remove_confirmation: Option<String>,
    notice: Option<String>,
    last_poll: Instant,
    needs_seed: bool,
    structure_dirty: bool,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    local_doc: AutoCommit,
    isolate_document: bool,
    /// API keys are deliberately process-local masked-input buffers. They are
    /// not Automerge text, never enter the settings document, and are erased
    /// as soon as the gateway accepts or rejects submission.
    provider_key_buffers: Arc<Mutex<HashMap<String, Arc<Mutex<String>>>>>,
    provider_field_buffers: Arc<Mutex<HashMap<String, HostedFieldBuffers>>>,
    provider_notice: ui::LocalState<Option<String>>,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl AgentModelsPane {
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
        self.isolate_document = true;
        self
    }

    pub fn new() -> Self {
        let mut pane = Self::with_control(Arc::new(ModelManagerCli));
        pane.sync_provider_profile = true;
        sync_local_profile(&pane.snapshot.models);
        pane
    }

    pub fn with_control(control: Arc<dyn LocalModelControl>) -> Self {
        let (snapshot, manager_available, notice) = match control.snapshot() {
            Ok(snapshot) => (snapshot, true, None),
            Err(error) => (
                fallback_snapshot(),
                false,
                Some(format!(
                    "The local model service is unavailable ({error}). Install or start tilo-model-manager to inspect hardware, install models, or manage local model lifecycle."
                )),
            ),
        };
        Self {
            control,
            snapshot,
            manager_available,
            sync_provider_profile: false,
            confirmation: None,
            remove_confirmation: None,
            notice,
            last_poll: ui::time::now(),
            needs_seed: true,
            structure_dirty: false,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            provider_key_buffers: Arc::new(Mutex::new(HashMap::new())),
            provider_field_buffers: Arc::new(Mutex::new(HashMap::new())),
            provider_notice: ui::LocalState::new(None),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn refresh(&mut self) {
        match self.control.snapshot() {
            Ok(snapshot) => {
                if self.sync_provider_profile {
                    sync_local_profile(&snapshot.models);
                }
                self.snapshot = snapshot;
                self.manager_available = true;
                self.notice = None;
            }
            Err(error) => {
                self.manager_available = false;
                self.notice = Some(format!("Could not refresh local models: {error}"));
            }
        }
        self.last_poll = ui::time::now();
        self.structure_dirty = true;
    }

    fn operation_is_running(&self, operation: &crate::actions::AgentModelOperation) -> bool {
        let token = crate::actions::local_operation_token_for(
            crate::actions::AGENT_MODEL_OPERATION_TYPE,
            operation,
        );
        matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: active } if active == token
        )
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

    fn is_busy(&self) -> bool {
        self.snapshot
            .models
            .iter()
            .any(|model| model.phase.is_transient())
            || self
                .snapshot
                .accelerator_setup
                .as_ref()
                .is_some_and(|setup| setup.status.is_transient())
    }

    fn accelerator_card(&self) -> Option<El> {
        let setup = self.snapshot.accelerator_setup.as_ref()?;
        let mut body = vec![value_row("Signed hardware match", &setup.boot_pack_id)];
        let status = match &setup.status {
            AcceleratorSetupState::Available => {
                body.push(caption(
                    "Tilo found a signed accelerator stack matched to this device. Setup stages a new immutable system image; it does not install a driver into the running system.",
                ));
                body.push(Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "agent-models-accelerator-setup",
                            "Set up accelerator",
                            Some(ACCELERATOR_SETUP_KEY),
                        )))
                        .push(Box::new(Spacer::new())),
                ));
                "Ready to set up"
            }
            AcceleratorSetupState::Queued => {
                body.push(Box::new(
                    ProgressBar::indeterminate().with_label("Preparing accelerator support"),
                ));
                body.push(caption(
                    "The request is queued. You can leave Settings while Tilo verifies and stages the signed system image.",
                ));
                "Queued"
            }
            AcceleratorSetupState::Staging => {
                body.push(Box::new(
                    ProgressBar::indeterminate().with_label("Staging accelerator support"),
                ));
                body.push(caption(
                    "Tilo is building the next boot deployment. The current desktop keeps running normally.",
                ));
                "Staging"
            }
            AcceleratorSetupState::RestartRequired => {
                body.push(caption(
                    "Accelerator support is ready for the next boot. Restart from the system power menu when convenient; Tilo will verify the booted deployment before calling it active.",
                ));
                "Restart required"
            }
            AcceleratorSetupState::Active => {
                body.push(caption(
                    "The signed accelerator stack is active and local model setup can use it.",
                ));
                "Active"
            }
            AcceleratorSetupState::Error { message } => {
                body.push(caption(message));
                body.push(caption(
                    "Nothing was installed into the running system. Retry rechecks this device against the signed hardware policy.",
                ));
                body.push(Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "agent-models-accelerator-retry",
                            "Retry accelerator setup",
                            Some(ACCELERATOR_SETUP_KEY),
                        )))
                        .push(Box::new(Spacer::new())),
                ));
                "Needs attention"
            }
        };
        Some(card_with_action(
            "Accelerator support",
            Box::new(Text::new(status).with_size(12.0)),
            rows(body),
        ))
    }

    fn recommendation_card(&self) -> El {
        let hardware = &self.snapshot.hardware;
        let mut body = vec![
            value_row("Hardware fit", hardware.fit.label()),
            value_row("System memory", &format!("{} GiB", hardware.memory_gib)),
            value_row("Accelerator", &hardware.accelerator),
        ];
        if let Some(vram) = hardware.accelerator_memory_gib {
            body.push(value_row("Accelerator memory", &format!("{vram} GiB")));
        }
        if let Some(storage) = hardware.free_storage_gib {
            body.push(value_row("Storage available", &format!("{storage} GiB")));
        }
        body.push(caption(&hardware.reason));
        let Some(package) = &self.snapshot.recommendation else {
            body.push(value_row(
                "Recommended model",
                "Waiting for signed model catalog",
            ));
            body.push(caption(
                "Setup stays disabled until the local model service reports a verified, immutable model release.",
            ));
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new("agent-models-setup-unavailable", "Set up unavailable", None)
                            .with_enabled(false),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
            return card_with_action(
                "Recommended for this device",
                Box::new(Text::new("Service unavailable").with_size(12.0)),
                rows(body),
            );
        };
        body.extend([
            value_row("Recommended model", &package.name),
            caption(&package.description),
        ]);
        if self.confirmation.as_deref() == Some(&package.id) {
            body.extend([
                value_row("Download", &byte_size(package.download_bytes)),
                value_row("Storage after install", &byte_size(package.installed_bytes)),
                value_row("Source", &package.source),
                value_row("Pinned revision", &package.revision),
                value_row("License", &package.license),
                caption(&format!(
                    "By continuing, you accept the model license at {}. Tilo downloads the pinned snapshot once and keeps inference on this device.",
                    package.license_url
                )),
                Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "agent-models-confirm-install",
                            "Accept license & download",
                            Some(CONFIRM_KEY),
                        )))
                        .push(Box::new(Button::new(
                            "agent-models-cancel-install",
                            "Cancel",
                            Some(CANCEL_KEY),
                        )))
                        .push(Box::new(Spacer::new())),
                ),
            ]);
        } else {
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new(
                            "agent-models-setup-local",
                            "Set up on this device",
                            Some(SETUP_KEY),
                        )
                        .with_enabled(
                            self.manager_available
                                && hardware.fit != HardwareFit::Unsupported
                                && self
                                    .snapshot
                                    .accelerator_setup
                                    .as_ref()
                                    .is_none_or(|setup| !setup.status.blocks_model_setup()),
                        ),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
        }
        card_with_action(
            "Recommended for this device",
            Box::new(Text::new(hardware.fit.label()).with_size(12.0)),
            rows(body),
        )
    }

    fn model_card(&self, model: &InstalledLocalModel) -> El {
        let mut body = vec![
            value_row("Status", model.phase.status()),
            value_row("Storage", &byte_size(model.package.installed_bytes)),
            value_row("License", &model.package.license),
        ];
        match &model.phase {
            LocalModelPhase::Downloading {
                downloaded_bytes,
                total_bytes,
            } => {
                body.push(Box::new(
                    ProgressBar::new(phase_progress(&model.phase).unwrap_or(0.0))
                        .with_label(format!("Downloading {}", model.package.name)),
                ));
                body.push(caption(&format!(
                    "{} of {} downloaded · safe to leave this page",
                    byte_size(*downloaded_bytes),
                    byte_size(*total_bytes)
                )));
            }
            LocalModelPhase::Installing { percent, message } => {
                body.push(Box::new(match percent {
                    Some(_) => ProgressBar::new(phase_progress(&model.phase).unwrap_or(0.0))
                        .with_label(format!("Installing {}", model.package.name)),
                    None => ProgressBar::indeterminate()
                        .with_label(format!("Installing {}", model.package.name)),
                }));
                body.push(caption(message));
            }
            LocalModelPhase::Error { message } => {
                body.push(caption(message));
                body.push(caption(
                    "Retry resumes verified files where possible. Remove clears this model's local snapshot.",
                ));
            }
            LocalModelPhase::Ready { running: true, .. } => body.push(caption(
                "Available to agents through the local Tilo gateway. Workspace permissions still apply to every task.",
            )),
            LocalModelPhase::Ready { running: false, .. } => body.push(caption(
                "Installed and verified. Start it when you want agents to use it; stopped models consume no accelerator memory.",
            )),
        }
        let actions = available_actions(&model.phase);
        if !actions.is_empty() {
            let mut row = Row::new();
            for action in actions {
                let label = if action == ModelAction::Remove
                    && self.remove_confirmation.as_deref() == Some(&model.package.id)
                {
                    "Confirm remove"
                } else {
                    action_label(action)
                };
                row = row.push(Box::new(Button::new(
                    &format!(
                        "agent-models-{}-{}",
                        action_slug(action),
                        model_key(&model.package.id, action)
                    ),
                    label,
                    Some(&model_key(&model.package.id, action)),
                )));
            }
            body.push(Box::new(row.push(Box::new(Spacer::new()))));
        }
        card_with_action(
            &model.package.name,
            Box::new(Text::new(model.phase.status()).with_size(12.0)),
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
    fn hosted_profiles(&self) -> El {
        let config = ProviderConfig::load();
        let action_resources = crate::actions::agent_provider_action_resources();
        let mut body: Vec<El> = vec![caption(
            "Hosted and compatible endpoints are personal to this device. Credentials remain references in the system vault.",
        )];
        body.push(Box::new(
            Row::new()
                .push(Box::new(provider_action_button(
                    &action_resources,
                    "agent-models-add-anthropic",
                    "Add Anthropic",
                    None,
                    "add-anthropic",
                )))
                .push(hgap(8.0))
                .push(Box::new(provider_action_button(
                    &action_resources,
                    "agent-models-add-openai",
                    "Add compatible API",
                    None,
                    "add-openai-compatible",
                )))
                .push(Box::new(Spacer::new())),
        ));
        for profile in config
            .profiles
            .iter()
            .filter(|profile| profile.location != EndpointLocation::ThisDevice)
        {
            body.push(value_row(profile.display_label(), profile.location.label()));
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
            body.push(Box::new(
                TextInput::local()
                    .with_id(&format!("agent-models-label-{profile_id}"))
                    .with_external_buffer(Arc::clone(&fields.label))
                    .with_initial_value(&label)
                    .with_placeholder("Provider name"),
            ));
            body.push(Box::new(
                TextInput::local()
                    .with_id(&format!("agent-models-url-{profile_id}"))
                    .with_external_buffer(Arc::clone(&fields.endpoint))
                    .with_initial_value(&endpoint)
                    .with_placeholder("https://provider.example/v1"),
            ));
            body.push(Box::new(
                TextInput::local()
                    .with_id(&format!("agent-models-model-{profile_id}"))
                    .with_external_buffer(Arc::clone(&fields.model))
                    .with_initial_value(&model)
                    .with_placeholder("Model ID"),
            ));
            let save_id = profile_id.clone();
            let save_fields = fields.clone();
            let save_notice = self.provider_notice.clone();
            let alternate_protocol = match profile.protocol {
                ProviderProtocol::Anthropic => ProviderProtocol::OpenAiCompatible,
                ProviderProtocol::OpenAiCompatible => ProviderProtocol::Anthropic,
            };
            let alternate_protocol_label = format!("Use {}", alternate_protocol.label());
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new(
                            &format!("agent-models-save-provider-{profile_id}"),
                            "Save details",
                            None,
                        )
                        .on_effect(move || {
                            let label = save_fields
                                .label
                                .lock()
                                .map(|value| value.clone())
                                .unwrap_or_default();
                            let endpoint = save_fields
                                .endpoint
                                .lock()
                                .map(|value| value.clone())
                                .unwrap_or_default();
                            let model = save_fields
                                .model
                                .lock()
                                .map(|value| value.clone())
                                .unwrap_or_default();
                            let mut config = ProviderConfig::load();
                            let result = if let Some(profile) = config
                                .profiles
                                .iter_mut()
                                .find(|profile| profile.id == save_id)
                            {
                                profile.label = label;
                                profile.endpoint = endpoint;
                                if !model.trim().is_empty() {
                                    if !profile.models.iter().any(|candidate| candidate.id == model)
                                    {
                                        profile.models.push(ModelDescriptor::new(model.clone()));
                                    }
                                    profile.default_model = Some(model);
                                }
                                config.save().map_err(|error| error.to_string())
                            } else {
                                Err("Provider is no longer available.".into())
                            };
                            save_notice.set(Some(match result {
                                Ok(()) => "Provider details saved.".into(),
                                Err(error) => format!("Provider details were not saved: {error}"),
                            }));
                        }),
                    ))
                    .push(hgap(8.0))
                    .push(Box::new(provider_action_button(
                        &action_resources,
                        &format!("agent-models-protocol-{profile_id}"),
                        &alternate_protocol_label,
                        Some(&profile_id),
                        "set-protocol",
                    )))
                    .push(hgap(8.0))
                    .push(Box::new(provider_action_button(
                        &action_resources,
                        &format!("agent-models-enabled-{profile_id}"),
                        if profile.enabled { "Disable" } else { "Enable" },
                        Some(&profile_id),
                        if profile.enabled { "disable" } else { "enable" },
                    )))
                    .push(hgap(8.0))
                    .push(Box::new(provider_action_button(
                        &action_resources,
                        &format!("agent-models-remove-provider-{profile_id}"),
                        "Remove provider",
                        Some(&profile_id),
                        "remove",
                    )))
                    .push(Box::new(Spacer::new())),
            ));
            body.push(value_row("Protocol", profile.protocol.label()));
            let buffer = self.key_buffer(&profile_id);
            let initial = buffer.lock().map(|key| key.clone()).unwrap_or_default();
            let set_id = profile_id.clone();
            let set_buffer = Arc::clone(&buffer);
            let set_notice = self.provider_notice.clone();
            let remove_id = profile_id.clone();
            let remove_notice = self.provider_notice.clone();
            let configured = tilo_model_gateway::secret_configured(&profile_id).unwrap_or(false);
            body.push(Box::new(
                Row::new()
                    .push(Box::new(
                        TextInput::local()
                            .with_id(&format!("agent-models-api-key-{profile_id}"))
                            .with_external_buffer(buffer)
                            .with_initial_value(&initial)
                            .with_secret(true)
                            .with_placeholder("API key (stored in Tilo vault)"),
                    ))
                    .push(Box::new(
                        Button::new(
                            &format!("agent-models-set-api-key-{profile_id}"),
                            if configured {
                                "Replace key"
                            } else {
                                "Save key"
                            },
                            None,
                        )
                        .on_effect(move || {
                            let mut key = set_buffer
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let value = std::mem::take(&mut *key);
                            let result = tilo_model_gateway::set_api_key(&set_id, value);
                            key.zeroize();
                            drop(key);
                            set_notice.set(Some(match result {
                                Ok(()) => "API key saved in the Tilo vault.".into(),
                                Err(error) => format!("API key was not saved: {error}"),
                            }));
                        }),
                    ))
                    .push(Box::new(
                        Button::new(
                            &format!("agent-models-remove-api-key-{profile_id}"),
                            "Remove key",
                            None,
                        )
                        .on_effect(move || {
                            let result = tilo_model_gateway::remove_api_key(&remove_id);
                            remove_notice.set(Some(match result {
                                Ok(()) => "API key removed from the Tilo vault.".into(),
                                Err(error) => format!("API key was not removed: {error}"),
                            }));
                        }),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
            body.push(caption(if configured {
                "API key is stored in the Tilo vault."
            } else {
                "No API key stored."
            }));
            if profile.default_model.is_some() {
                body.push(Box::new(provider_action_button(
                    &action_resources,
                    &format!("agent-models-default-{profile_id}"),
                    if config.default_profile.as_deref() == Some(&profile.id) {
                        "Default for new tasks"
                    } else {
                        "Use by default"
                    },
                    Some(&profile_id),
                    "set-default",
                )));
            }
        }
        for issue in config.validation_errors() {
            body.push(caption(&format!(
                "Provider configuration: {}",
                issue.message
            )));
        }
        card("Hosted and compatible providers", rows(body))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut cards = vec![card(
            "Private by design",
            rows(vec![
                caption(
                    "Models configured here supply inference only. They do not grant an agent access to a workspace, app, file, message, or device.",
                ),
                value_row("Local inference", "Loopback-only Tilo model gateway"),
                value_row("Downloads", "Explicit, pinned, and visible here"),
            ]),
        )];
        if !self.manager_available {
            cards.push(card(
                "Local model service unavailable",
                caption(
                    "Install or start tilo-model-manager to inspect hardware, install models, or manage local model lifecycle. Existing hosted providers remain available below.",
                ),
            ));
            cards.push(self.hosted_profiles());
            return crate::widgets::cards(cards);
        }
        if let Some(notice) = &self.notice {
            cards.push(card("Local model service", caption(notice)));
        }
        if let Some(accelerator) = self.accelerator_card() {
            cards.push(accelerator);
        }
        cards.push(self.recommendation_card());
        if self.snapshot.models.is_empty() {
            cards.push(card(
                "Models on this device",
                caption(
                    "No local models are installed. Setup shows the download, storage, source, revision, and license before anything changes.",
                ),
            ));
        } else {
            for model in &self.snapshot.models {
                cards.push(self.model_card(model));
            }
        }
        cards.push(self.hosted_profiles());
        crate::widgets::cards(cards)
    }

    fn seed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.needs_seed {
            return;
        }
        for key in [SETUP_KEY, CONFIRM_KEY, CANCEL_KEY, ACCELERATOR_SETUP_KEY] {
            let _ = state.put(root, key, false);
        }
        for model in &self.snapshot.models {
            for action in available_actions(&model.phase) {
                let _ = state.put(root, model_key(&model.package.id, action), false);
            }
        }
        self.needs_seed = false;
    }

    fn run_model_action(&mut self, model: &InstalledLocalModel, action: ModelAction) {
        let operation = match action {
            ModelAction::Start => crate::actions::AgentModelOperation::Start {
                id: model.package.id.clone(),
            },
            ModelAction::Stop => crate::actions::AgentModelOperation::Stop {
                id: model.package.id.clone(),
            },
            ModelAction::Retry => crate::actions::AgentModelOperation::Retry {
                id: model.package.id.clone(),
                revision: model.package.revision.clone(),
            },
            ModelAction::Remove => crate::actions::AgentModelOperation::Remove {
                id: model.package.id.clone(),
            },
        };
        let result = match action {
            ModelAction::Start => self.control.start(&model.package.id),
            ModelAction::Stop => self.control.stop(&model.package.id),
            ModelAction::Retry => self.control.install(&model.package),
            ModelAction::Remove => {
                if self.remove_confirmation.as_deref() != Some(&model.package.id)
                    && !self.operation_is_running(&operation)
                {
                    self.remove_confirmation = Some(model.package.id.clone());
                    self.notice = Some(format!(
                        "Remove {}? This deletes its {} local snapshot. Tasks and workspace data are unchanged.",
                        model.package.name,
                        byte_size(model.package.installed_bytes)
                    ));
                    self.structure_dirty = true;
                    return;
                }
                self.remove_confirmation = None;
                self.control.remove(&model.package.id)
            }
        };
        if let Err(error) = result {
            self.operation_finished(
                &operation,
                Err(format!("{} failed: {error}", action_label(action))),
            );
            self.notice = Some(format!("{} failed: {error}", action_label(action)));
            self.structure_dirty = true;
        } else {
            self.refresh();
            self.operation_finished(
                &operation,
                Ok(format!(
                    "{} request accepted by the local model manager",
                    action_label(action)
                )),
            );
        }
    }

    fn sync_actions(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if take_bool(state, root, ACCELERATOR_SETUP_KEY) {
            let operation = crate::actions::AgentModelOperation::StageAccelerator;
            match self.control.stage_accelerator() {
                Ok(()) => {
                    self.refresh();
                    self.operation_finished(
                        &operation,
                        Ok("Accelerator setup request accepted by the local model manager".into()),
                    );
                }
                Err(error) => {
                    self.operation_finished(
                        &operation,
                        Err(format!("Accelerator setup could not start: {error}")),
                    );
                    self.notice = Some(format!("Accelerator setup could not start: {error}"));
                    self.structure_dirty = true;
                }
            }
        }
        if take_bool(state, root, SETUP_KEY)
            && let Some(package) = &self.snapshot.recommendation
        {
            self.confirmation = Some(package.id.clone());
            self.structure_dirty = true;
        }
        if take_bool(state, root, CANCEL_KEY) {
            self.confirmation = None;
            self.structure_dirty = true;
        }
        if take_bool(state, root, CONFIRM_KEY)
            && let Some(package) = self.snapshot.recommendation.clone()
            && (self.confirmation.as_deref() == Some(package.id.as_str())
                || self.operation_is_running(&crate::actions::AgentModelOperation::Install {
                    id: package.id.clone(),
                    revision: package.revision.clone(),
                }))
        {
            self.confirmation = None;
            let operation = crate::actions::AgentModelOperation::Install {
                id: package.id.clone(),
                revision: package.revision.clone(),
            };
            match self.control.install(&package) {
                Ok(()) => {
                    self.refresh();
                    self.operation_finished(
                        &operation,
                        Ok("Model installation request accepted by the local model manager".into()),
                    );
                }
                Err(error) => {
                    self.operation_finished(
                        &operation,
                        Err(format!("Installation could not start: {error}")),
                    );
                    self.notice = Some(format!("Installation could not start: {error}"));
                    self.structure_dirty = true;
                }
            }
        }
        let models = self.snapshot.models.clone();
        for model in &models {
            for action in available_actions(&model.phase) {
                let key = model_key(&model.package.id, action);
                if take_bool(state, root, &key) {
                    self.run_model_action(model, action);
                }
            }
        }
    }
}

impl Default for AgentModelsPane {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AgentModelsPane {
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

pub fn build() -> Box<dyn Widget> {
    Box::new(AgentModelsPane::new())
}

impl Widget for AgentModelsPane {
    fn sync(&mut self, shared: &mut AppModel, root: &ModelPath) {
        let (shared, _) = ui::doc::document(shared, root);
        let isolated = self.isolate_document;
        let mut state = if isolated {
            std::mem::take(&mut self.local_doc)
        } else {
            std::mem::take(shared)
        };
        let root = automerge::ROOT;
        if let Some(notice) = self.provider_notice.get() {
            self.provider_notice.set(None);
            self.notice = Some(notice);
            self.structure_dirty = true;
        }
        self.seed(&mut state, &root);
        if let Some(crate::actions::LocalPaneCommand::AgentModel(operation)) =
            self.command_state.get()
        {
            self.command_state.set(None);
            let key = match operation {
                crate::actions::AgentModelOperation::StageAccelerator => {
                    ACCELERATOR_SETUP_KEY.into()
                }
                crate::actions::AgentModelOperation::Install { .. } => CONFIRM_KEY.into(),
                crate::actions::AgentModelOperation::Start { id } => {
                    model_key(&id, ModelAction::Start)
                }
                crate::actions::AgentModelOperation::Stop { id } => {
                    model_key(&id, ModelAction::Stop)
                }
                crate::actions::AgentModelOperation::Retry { id, .. } => {
                    model_key(&id, ModelAction::Retry)
                }
                crate::actions::AgentModelOperation::Remove { id } => {
                    model_key(&id, ModelAction::Remove)
                }
            };
            let _ = state.put(&root, key, true);
        }
        self.sync_actions(&mut state, &root);
        if let Some(composed) = &mut self.composed {
            composed.sync(&mut state, &root);
        }
        if isolated {
            self.local_doc = state;
        } else {
            *shared = state;
        }
    }

    fn update(&mut self, _state: &mut AppModel, _path: &ModelPath) -> bool {
        if self.is_busy() && self.last_poll.elapsed() >= POLL_INTERVAL {
            self.refresh();
            return true;
        }
        self.is_busy()
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(composed) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child) = children.first()
        {
            composed.handle_event(event, &mut ctx.reborrow(ctx.path, child));
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
        "AgentModelsPane"
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

fn take_bool(state: &mut AutoCommit, root: &ObjId, key: &str) -> bool {
    let pressed = matches!(
        state.get(root, key),
        Ok(Some((Value::Scalar(value), _)))
            if matches!(value.as_ref(), automerge::ScalarValue::Boolean(true))
    );
    if pressed {
        let _ = state.put(root, key, false);
    }
    pressed
}

#[cfg(test)]
mod tests {
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
        }
    }

    fn snapshot(phase: Option<LocalModelPhase>) -> LocalModelSnapshot {
        let package = package();
        LocalModelSnapshot {
            hardware: HardwareRecommendation {
                fit: HardwareFit::Good,
                memory_gib: 24,
                accelerator: "GPU · 12 GiB".into(),
                accelerator_memory_gib: Some(12),
                free_storage_gib: Some(80),
                reason: "Balanced for this device".into(),
            },
            recommendation: Some(package.clone()),
            models: phase
                .map(|phase| vec![InstalledLocalModel { package, phase }])
                .unwrap_or_default(),
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

    /// No fabricated recommendations, license-confirmed installs, bounded progress and confirmed removal.
    #[test]
    fn model_catalog_install_and_removal_are_honest_and_confirmed() {
        // Case `unavailable_manager_never_fabricates_a_catalog_recommendation`.
        {
            let fallback = fallback_snapshot();
            assert!(fallback.recommendation.is_none());

            let control = Arc::new(MockControl {
                snapshot: Mutex::new(fallback),
                calls: Mutex::new(Vec::new()),
            });
            let pane = AgentModelsPane::with_control(control);
            let composition = pane.build_composition();
            let mut nodes = Vec::new();
            collect_accessibility(composition.as_ref(), &mut nodes);
            let setup = nodes
                .iter()
                .find(|node| node.name.as_deref() == Some("Set up unavailable"))
                .expect("disabled setup remains understandable to assistive technology");
            assert!(setup.state.disabled);
        }

        // Case `install_requires_visible_license_confirmation`.
        {
            let control = Arc::new(MockControl {
                snapshot: Mutex::new(snapshot(None)),
                calls: Mutex::new(Vec::new()),
            });
            let mut pane = AgentModelsPane::with_control(control.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);

            let _ = doc.put(&root, SETUP_KEY, true);
            pane.sync(&mut doc, &root);
            assert_eq!(pane.confirmation.as_deref(), Some("test/model"));
            assert!(control.calls.lock().unwrap().is_empty());

            let _ = doc.put(&root, CONFIRM_KEY, true);
            pane.sync(&mut doc, &root);
            assert_eq!(
                control.calls.lock().unwrap().as_slice(),
                ["install:test/model@0123456789abcdef"]
            );
        }

        // Case `progress_is_bounded_and_machine_readable`.
        {
            let phase = LocalModelPhase::Downloading {
                downloaded_bytes: 3,
                total_bytes: 4,
            };
            assert_eq!(phase_progress(&phase), Some(0.75));
            let progress = ProgressBar::new(phase_progress(&phase).unwrap()).with_label("Download");
            let access = Widget::accessibility(&progress).expect("labelled progress is accessible");
            assert_eq!(access.value.as_deref(), Some("75%"));
        }

        // Case `remove_is_confirmed_before_backend_mutation`.
        {
            let control = Arc::new(MockControl {
                snapshot: Mutex::new(snapshot(Some(LocalModelPhase::Ready {
                    running: false,
                    endpoint: None,
                }))),
                calls: Mutex::new(Vec::new()),
            });
            let mut pane = AgentModelsPane::with_control(control.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let key = model_key("test/model", ModelAction::Remove);

            let _ = doc.put(&root, &key, true);
            pane.sync(&mut doc, &root);
            assert!(control.calls.lock().unwrap().is_empty());
            assert_eq!(pane.remove_confirmation.as_deref(), Some("test/model"));

            let _ = doc.put(&root, &key, true);
            pane.sync(&mut doc, &root);
            assert_eq!(
                control.calls.lock().unwrap().as_slice(),
                ["remove:test/model"]
            );
        }
    }

    /// Terminal receipts, safe lifecycle controls, separate accelerator setup and the manager endpoint.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn local_model_lifecycle_exposes_only_safe_controls() {
        // Case `local_start_reports_a_terminal_receipt_only_for_its_matching_operation`.
        {
            let control = Arc::new(MockControl {
                snapshot: Mutex::new(snapshot(Some(LocalModelPhase::Ready {
                    running: false,
                    endpoint: None,
                }))),
                calls: Mutex::new(Vec::new()),
            });
            let status = ui::LocalState::new(crate::actions::LocalOperationStatus::Idle);
            let mut pane =
                AgentModelsPane::with_control(control.clone()).with_operation_status(&status);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let operation = crate::actions::AgentModelOperation::Start {
                id: "test/model".into(),
            };
            status.set(crate::actions::LocalOperationStatus::Running {
                token: crate::actions::local_operation_token_for(
                    crate::actions::AGENT_MODEL_OPERATION_TYPE,
                    &operation,
                ),
            });
            let _ = doc.put(&root, model_key("test/model", ModelAction::Start), true);
            pane.sync(&mut doc, &root);
            assert_eq!(
                control.calls.lock().unwrap().as_slice(),
                ["start:test/model"]
            );
            assert!(matches!(
                status.get(),
                crate::actions::LocalOperationStatus::Succeeded { .. }
            ));
        }

        // Case `lifecycle_exposes_only_safe_controls`.
        {
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
        }

        // Case `accelerator_setup_is_separate_from_model_lifecycle`.
        {
            let control = Arc::new(MockControl {
                snapshot: Mutex::new(LocalModelSnapshot {
                    accelerator_setup: Some(AcceleratorSetup {
                        boot_pack_id: "gpu-stack".into(),
                        status: AcceleratorSetupState::Available,
                    }),
                    ..snapshot(None)
                }),
                calls: Mutex::new(Vec::new()),
            });
            let mut pane = AgentModelsPane::with_control(control.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);

            let _ = doc.put(&root, ACCELERATOR_SETUP_KEY, true);
            pane.sync(&mut doc, &root);
            assert_eq!(
                control.calls.lock().unwrap().as_slice(),
                ["stage-accelerator"]
            );
        }

        // Case `local_profile_uses_the_ready_manager_endpoint_not_port_8000`.
        {
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
        }
    }
}
