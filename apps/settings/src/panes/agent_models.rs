//! Agent Models — device-local model lifecycle and provider profiles.
//!
//! qOS owns the user-facing lifecycle, while a small adapter speaks to the
//! privileged model manager that pins Hugging Face snapshots and supervises
//! vLLM. Settings never downloads model bytes or starts a server itself.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
};
use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, ProgressBar, Row, Spacer, Text, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::widgets::{El, caption, card, card_with_action, hgap, rows, value_row};

const SETUP_KEY: &str = "agent_models.local.setup";
const CONFIRM_KEY: &str = "agent_models.local.confirm";
const CANCEL_KEY: &str = "agent_models.local.cancel";
const POLL_INTERVAL: Duration = Duration::from_millis(750);

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
    RestartRequired {
        reason: String,
    },
    Ready {
        running: bool,
    },
    Error {
        message: String,
    },
}

impl LocalModelPhase {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Downloading { .. } | Self::Installing { .. })
    }

    fn status(&self) -> &'static str {
        match self {
            Self::Downloading { .. } => "Downloading",
            Self::Installing { .. } => "Installing",
            Self::RestartRequired { .. } => "Restart required",
            Self::Ready { running: true } => "Running",
            Self::Ready { running: false } => "Ready",
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
pub struct LocalModelSnapshot {
    pub hardware: HardwareRecommendation,
    pub recommendation: Option<LocalModelPackage>,
    #[serde(default)]
    pub models: Vec<InstalledLocalModel>,
}

/// Boundary implemented by qOS' model service. The production adapter is a
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
    fn restart_device(&self, id: &str) -> Result<(), String>;
}

pub struct ModelManagerCli;

impl ModelManagerCli {
    fn command(verb: &str, id: &str) -> Result<(), String> {
        let status = Command::new("qos-model-manager")
            .args([verb, "--model", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map_err(|error| format!("model service is unavailable: {error}"))?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("model service refused {verb}"))
    }
}

impl LocalModelControl for ModelManagerCli {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        let output = Command::new("qos-model-manager")
            .args(["snapshot", "--format", "json"])
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("model service is unavailable: {error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("model service returned invalid status: {error}"))
    }

    fn install(&self, model: &LocalModelPackage) -> Result<(), String> {
        let status = Command::new("qos-model-manager")
            .args([
                "install",
                "--model",
                &model.id,
                "--revision",
                &model.revision,
                "--accept-license",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map_err(|error| format!("model service is unavailable: {error}"))?;
        status
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
    fn restart_device(&self, id: &str) -> Result<(), String> {
        Self::command("request-restart", id)
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
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelAction {
    Start,
    Stop,
    Remove,
    Retry,
    Restart,
}

fn available_actions(phase: &LocalModelPhase) -> Vec<ModelAction> {
    match phase {
        LocalModelPhase::Ready { running: true } => vec![ModelAction::Stop, ModelAction::Remove],
        LocalModelPhase::Ready { running: false } => {
            vec![ModelAction::Start, ModelAction::Remove]
        }
        LocalModelPhase::Error { .. } => vec![ModelAction::Retry, ModelAction::Remove],
        LocalModelPhase::RestartRequired { .. } => vec![ModelAction::Restart],
        LocalModelPhase::Downloading { .. } | LocalModelPhase::Installing { .. } => Vec::new(),
    }
}

fn action_label(action: ModelAction) -> &'static str {
    match action {
        ModelAction::Start => "Start",
        ModelAction::Stop => "Stop",
        ModelAction::Remove => "Remove…",
        ModelAction::Retry => "Retry install",
        ModelAction::Restart => "Restart device…",
    }
}

fn action_slug(action: ModelAction) -> &'static str {
    match action {
        ModelAction::Start => "start",
        ModelAction::Stop => "stop",
        ModelAction::Remove => "remove",
        ModelAction::Retry => "retry",
        ModelAction::Restart => "restart",
    }
}

fn model_key(id: &str, action: ModelAction) -> String {
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

fn save_profile(profile: ProviderProfile) {
    let mut config = ProviderConfig::load();
    config.upsert(profile);
    let _ = config.save();
}

fn select_default(profile_id: String) {
    let mut config = ProviderConfig::load();
    if config
        .profiles
        .iter()
        .any(|profile| profile.id == profile_id && profile.default_model.is_some())
    {
        config.default_profile = Some(profile_id);
        let _ = config.save();
    }
}

fn sync_local_profile(models: &[InstalledLocalModel]) {
    let ready = models
        .iter()
        .filter(|model| matches!(model.phase, LocalModelPhase::Ready { .. }))
        .collect::<Vec<_>>();
    if ready.is_empty() {
        return;
    }
    let mut config = ProviderConfig::load();
    let existing = config
        .profiles
        .iter()
        .position(ProviderProfile::is_local_vllm_candidate);
    let mut profile = existing
        .and_then(|index| config.profiles.get(index).cloned())
        .unwrap_or_else(ProviderProfile::local_vllm);
    profile.models = ready
        .iter()
        .map(|model| ModelDescriptor {
            id: model.package.id.clone(),
            label: model.package.name.clone(),
            context_window: None,
            vision: false,
            tool_calling: true,
        })
        .collect();
    profile.default_model = profile
        .default_model
        .filter(|id| profile.models.iter().any(|model| model.id == *id))
        .or_else(|| profile.models.first().map(|model| model.id.clone()));
    profile.enabled = ready
        .iter()
        .any(|model| matches!(model.phase, LocalModelPhase::Ready { running: true }));
    let changed = existing.and_then(|index| config.profiles.get(index)) != Some(&profile);
    if changed {
        config.upsert(profile);
        let _ = config.save();
    }
}

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
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl AgentModelsPane {
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
                    "Local model service is not connected. Hardware guidance is approximate; installation controls become available when qos-model-manager is installed. {error}"
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
            last_poll: Instant::now(),
            needs_seed: true,
            structure_dirty: false,
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
        self.last_poll = Instant::now();
        self.structure_dirty = true;
    }

    fn is_busy(&self) -> bool {
        self.snapshot
            .models
            .iter()
            .any(|model| model.phase.is_transient())
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
                    "By continuing, you accept the model license at {}. qOS downloads the pinned snapshot once and keeps inference on this device.",
                    package.license_url
                )),
                Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "agent-models-confirm-install",
                            "Accept license & download",
                            Some(CONFIRM_KEY),
                        )))
                        .push(hgap(8.0))
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
                            self.manager_available && hardware.fit != HardwareFit::Unsupported,
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
            LocalModelPhase::RestartRequired { reason } => {
                body.push(caption(reason));
                body.push(caption(
                    "Your download is complete. Restarting activates the required runtime or driver; no model data is downloaded again.",
                ));
            }
            LocalModelPhase::Error { message } => {
                body.push(caption(message));
                body.push(caption(
                    "Retry resumes verified files where possible. Remove clears this model's local snapshot.",
                ));
            }
            LocalModelPhase::Ready { running: true } => body.push(caption(
                "Available to agents through the local qOS gateway. Workspace permissions still apply to every task.",
            )),
            LocalModelPhase::Ready { running: false } => body.push(caption(
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
                row = row
                    .push(Box::new(Button::new(
                        &format!(
                            "agent-models-{}-{}",
                            action_slug(action),
                            model_key(&model.package.id, action)
                        ),
                        label,
                        Some(&model_key(&model.package.id, action)),
                    )))
                    .push(hgap(8.0));
            }
            body.push(Box::new(row.push(Box::new(Spacer::new()))));
        }
        card_with_action(
            &model.package.name,
            Box::new(Text::new(model.phase.status()).with_size(12.0)),
            rows(body),
        )
    }

    fn hosted_profiles() -> El {
        let config = ProviderConfig::load();
        let mut body: Vec<El> = vec![caption(
            "Hosted and compatible endpoints are personal to this device. Credentials remain references in the system vault.",
        )];
        body.push(Box::new(
            Row::new()
                .push(Box::new(
                    Button::new("agent-models-add-anthropic", "Add Anthropic", None).on_effect(
                        || save_profile(ProviderProfile::hosted(ProviderProtocol::Anthropic)),
                    ),
                ))
                .push(hgap(8.0))
                .push(Box::new(
                    Button::new("agent-models-add-openai", "Add compatible API", None).on_effect(
                        || {
                            save_profile(ProviderProfile::hosted(
                                ProviderProtocol::OpenAiCompatible,
                            ));
                        },
                    ),
                ))
                .push(Box::new(Spacer::new())),
        ));
        for profile in config
            .profiles
            .iter()
            .filter(|profile| profile.location != EndpointLocation::ThisDevice)
        {
            body.push(value_row(profile.display_label(), profile.location.label()));
            if profile.default_model.is_some() {
                let profile_id = profile.id.clone();
                body.push(Box::new(
                    Button::new(
                        &format!("agent-models-default-{profile_id}"),
                        if config.default_profile.as_deref() == Some(&profile.id) {
                            "Default for new tasks"
                        } else {
                            "Use by default"
                        },
                        None,
                    )
                    .on_effect(move || select_default(profile_id.clone())),
                ));
            }
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
                value_row("Local inference", "Loopback-only qOS model gateway"),
                value_row("Downloads", "Explicit, pinned, and visible here"),
            ]),
        )];
        if let Some(notice) = &self.notice {
            cards.push(card("Local model service", caption(notice)));
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
        cards.push(Self::hosted_profiles());
        crate::widgets::cards(cards)
    }

    fn seed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.needs_seed {
            return;
        }
        for key in [SETUP_KEY, CONFIRM_KEY, CANCEL_KEY] {
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
        let result = match action {
            ModelAction::Start => self.control.start(&model.package.id),
            ModelAction::Stop => self.control.stop(&model.package.id),
            ModelAction::Retry => self.control.install(&model.package),
            ModelAction::Restart => self.control.restart_device(&model.package.id),
            ModelAction::Remove => {
                if self.remove_confirmation.as_deref() != Some(&model.package.id) {
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
            self.notice = Some(format!("{} failed: {error}", action_label(action)));
            self.structure_dirty = true;
        } else {
            self.refresh();
        }
    }

    fn sync_actions(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if take_bool(state, root, SETUP_KEY) {
            if let Some(package) = &self.snapshot.recommendation {
                self.confirmation = Some(package.id.clone());
                self.structure_dirty = true;
            }
        }
        if take_bool(state, root, CANCEL_KEY) {
            self.confirmation = None;
            self.structure_dirty = true;
        }
        if take_bool(state, root, CONFIRM_KEY)
            && let Some(package) = self.snapshot.recommendation.clone()
            && self.confirmation.as_deref() == Some(package.id.as_str())
        {
            self.confirmation = None;
            match self.control.install(&package) {
                Ok(()) => self.refresh(),
                Err(error) => {
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

pub fn build() -> Box<dyn Widget> {
    Box::new(AgentModelsPane::new())
}

impl Widget for AgentModelsPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.seed(state, root);
        self.sync_actions(state, root);
        if let Some(composed) = &mut self.composed {
            composed.sync(state, root);
        }
    }

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
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

    #[allow(clippy::too_many_arguments)]
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
        fn restart_device(&self, id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("restart:{id}"));
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

    #[test]
    fn unavailable_manager_never_fabricates_a_catalog_recommendation() {
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

    #[test]
    fn install_requires_visible_license_confirmation() {
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

    #[test]
    fn lifecycle_exposes_only_safe_controls() {
        assert_eq!(
            available_actions(&LocalModelPhase::Ready { running: false }),
            [ModelAction::Start, ModelAction::Remove]
        );
        assert_eq!(
            available_actions(&LocalModelPhase::Ready { running: true }),
            [ModelAction::Stop, ModelAction::Remove]
        );
        assert!(
            available_actions(&LocalModelPhase::Downloading {
                downloaded_bytes: 4,
                total_bytes: 10,
            })
            .is_empty()
        );
        assert_eq!(
            available_actions(&LocalModelPhase::RestartRequired {
                reason: "driver".into()
            }),
            [ModelAction::Restart]
        );
    }

    #[test]
    fn progress_is_bounded_and_machine_readable() {
        let phase = LocalModelPhase::Downloading {
            downloaded_bytes: 3,
            total_bytes: 4,
        };
        assert_eq!(phase_progress(&phase), Some(0.75));
        let progress = ProgressBar::new(phase_progress(&phase).unwrap()).with_label("Download");
        let access = Widget::accessibility(&progress).expect("labelled progress is accessible");
        assert_eq!(access.value.as_deref(), Some("75%"));
    }

    #[test]
    fn remove_is_confirmed_before_backend_mutation() {
        let control = Arc::new(MockControl {
            snapshot: Mutex::new(snapshot(Some(LocalModelPhase::Ready { running: false }))),
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
