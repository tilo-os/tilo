//! Which model serves which task, and the one resolver every consumer and
//! Settings share.
//!
//! ## Precedence
//!
//! [`resolve`] takes `(consumer, task, agent)` and returns the first layer
//! that applies, most specific first:
//!
//! 1. **Agent** — an assignment for this agent and task
//!    ([`ModelAssignments::agents`]); only for tasks marked `per_agent`.
//! 2. **Task** — the person's assignment for the task
//!    ([`ModelAssignments::tasks`]).
//! 3. **Default** — the selected [`Preset`], derived for this device; or,
//!    with no preset, the task's own recommendation.
//!
//! An explicit layer that cannot be honoured (its provider was removed, its
//! model is not running, or it breaks the task's constraints) is an error,
//! never a silent fall-through: falling through could send a task's input
//! somewhere the person did not choose. The constraints — capabilities and
//! [`PrivacyClass`] — are checked on every layer, so no assignment and no
//! caller can move a device-only task off the device.
//!
//! ## Scope
//!
//! Assignments are **personal** (`agent_providers.assignments`): they express
//! the person's choice and follow them to each device. That includes the
//! per-agent layer, because an agent runs through the person's own gateway
//! and spends the person's own credentials; a workspace can admit an agent
//! but cannot choose which of the person's providers it bills. What varies
//! by device is resolved, not stored: a [`ModelTarget::Local`] names a
//! catalogue model and resolves against the device's runtime profile, and a
//! preset is derived from the device's hardware snapshot every time.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::local::{AcceleratorState, AcceleratorVendor, HardwareSnapshot};
use crate::tasks::{self, Consumer, PrivacyClass, TaskSpec, Unmet};
use crate::{
    ASSIGNMENTS_KEY, EndpointLocation, ModelCapability, ModelDescriptor, ModelOptions, OptionError,
    ProviderConfig, ProviderProfile, ReasoningEffort, SETTINGS_SECTION, is_pinned_revision,
};

/// What an assignment points at.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelTarget {
    /// A model of one of the person's provider profiles.
    Profile {
        profile_id: String,
        model_id: String,
    },
    /// A signed-catalogue model run by the model manager on whichever device
    /// resolves the task.
    Local { model_id: String },
}

/// One explicit choice for a task or an agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub target: ModelTarget,
    #[serde(default)]
    pub options: ModelOptions,
    /// The person's explicit allowance for a [`PrivacyClass::Sensitive`] task
    /// to be served off this device. Only a confirmed Settings action writes
    /// it; presets and recommendations never do.
    #[serde(default)]
    pub allow_off_device: bool,
}

impl Assignment {
    #[must_use]
    pub fn local(model_id: &str) -> Self {
        Self {
            target: ModelTarget::Local {
                model_id: model_id.to_string(),
            },
            options: ModelOptions::default(),
            allow_off_device: false,
        }
    }

    #[must_use]
    pub fn profile(profile_id: &str, model_id: &str) -> Self {
        Self {
            target: ModelTarget::Profile {
                profile_id: profile_id.to_string(),
                model_id: model_id.to_string(),
            },
            options: ModelOptions::default(),
            allow_off_device: false,
        }
    }
}

/// Recommended, device-adaptive bundles. A preset is not stored as
/// assignments: it is derived from the device and the providers on every
/// resolution, so it adapts when either changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    /// Everything on this device. Nothing is sent to a provider.
    Private,
    /// Private data on this device; open tasks on this device when it has a
    /// strong accelerator, otherwise on the default provider.
    Balanced,
    /// Open tasks on the default provider at high reasoning effort; private
    /// data still stays on this device.
    BestQuality,
}

impl Preset {
    pub const ALL: [Self; 3] = [Self::Private, Self::Balanced, Self::BestQuality];

    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Balanced => "balanced",
            Self::BestQuality => "best-quality",
        }
    }

    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.slug() == slug)
    }

    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Private => "ai-preset-private",
            Self::Balanced => "ai-preset-balanced",
            Self::BestQuality => "ai-preset-best-quality",
        }
    }

    #[must_use]
    pub const fn description_key(self) -> &'static str {
        match self {
            Self::Private => "ai-preset-private-description",
            Self::Balanced => "ai-preset-balanced-description",
            Self::BestQuality => "ai-preset-best-quality-description",
        }
    }
}

/// Accelerator memory, in MiB, from which Balanced runs open tasks locally.
pub const STRONG_ACCELERATOR_MIB: u64 = 15 * 1024;

/// The person's assignments. Personal scope; see the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelAssignments {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    /// Task id → assignment.
    pub tasks: BTreeMap<String, Assignment>,
    /// Agent id → task id → assignment.
    pub agents: BTreeMap<String, BTreeMap<String, Assignment>>,
}

impl ModelAssignments {
    pub fn load() -> Self {
        Self::from_section(&settings_store::load_section(SETTINGS_SECTION))
    }

    pub fn load_at(path: &Path) -> Self {
        Self::from_section(&settings_store::load_section_at(path, SETTINGS_SECTION))
    }

    fn from_section(section: &Map<String, Value>) -> Self {
        let mut assignments: Self = section
            .get(ASSIGNMENTS_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        // An assignment for a task this build does not serve is dropped, not
        // kept around to apply to a future task of the same name.
        assignments
            .tasks
            .retain(|task, _| tasks::task(task).is_some());
        for per_agent in assignments.agents.values_mut() {
            per_agent.retain(|task, _| tasks::task(task).is_some_and(|task| task.per_agent));
        }
        assignments
            .agents
            .retain(|_, per_agent| !per_agent.is_empty());
        assignments
    }

    /// # Errors
    ///
    /// Returns an error if the shared settings document cannot be updated.
    pub fn save(&self) -> std::io::Result<()> {
        settings_store::update_section(SETTINGS_SECTION, self.section()?)
    }

    /// # Errors
    ///
    /// Returns an error if the settings document at `path` cannot be updated.
    pub fn save_at(&self, path: &Path) -> std::io::Result<()> {
        settings_store::update_section_at(path, SETTINGS_SECTION, self.section()?)
    }

    fn section(&self) -> std::io::Result<Map<String, Value>> {
        let mut section = Map::new();
        section.insert(
            ASSIGNMENTS_KEY.to_string(),
            serde_json::to_value(self).map_err(std::io::Error::other)?,
        );
        Ok(section)
    }

    pub fn agent(&self, agent: &str, task: &str) -> Option<&Assignment> {
        self.agents.get(agent)?.get(task)
    }

    pub fn set_agent(&mut self, agent: &str, task: &str, assignment: Assignment) {
        self.agents
            .entry(agent.to_string())
            .or_default()
            .insert(task.to_string(), assignment);
    }

    pub fn clear_agent(&mut self, agent: &str, task: &str) {
        if let Some(per_agent) = self.agents.get_mut(agent) {
            per_agent.remove(task);
            if per_agent.is_empty() {
                self.agents.remove(agent);
            }
        }
    }

    /// Drop every assignment that names `profile_id`, when it is removed.
    pub fn forget_profile(&mut self, profile_id: &str) {
        let names = |assignment: &Assignment| matches!(&assignment.target, ModelTarget::Profile { profile_id: id, .. } if id == profile_id);
        self.tasks.retain(|_, assignment| !names(assignment));
        for per_agent in self.agents.values_mut() {
            per_agent.retain(|_, assignment| !names(assignment));
        }
        self.agents.retain(|_, per_agent| !per_agent.is_empty());
    }
}

/// Everything a resolution reads.
#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    /// The person's profiles with this device's runtime merged in.
    pub config: &'a ProviderConfig,
    pub assignments: &'a ModelAssignments,
    /// This device's hardware snapshot, when the model manager published one.
    pub hardware: Option<&'a HardwareSnapshot>,
}

/// Which layer produced a resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "layer", content = "preset", rename_all = "snake_case")]
pub enum Layer {
    Agent,
    Task,
    Preset(Preset),
    Recommended,
}

/// A concrete, constraint-checked answer: send this task to this model of
/// this profile, with these options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub task: &'static TaskSpec,
    /// The class this resolution honoured: the task's own, or stricter when
    /// the request said its content is more private ([`resolve_for`]).
    pub privacy: PrivacyClass,
    pub profile: ProviderProfile,
    pub model: ModelDescriptor,
    pub options: ModelOptions,
    pub layer: Layer,
}

/// Why a task cannot be served now. Every variant is safe to show and carries
/// no secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ResolveError {
    UnknownTask,
    /// The caller is not a declared consumer of the task.
    NotConsumer,
    /// The assigned provider was removed or is disabled.
    ProfileUnavailable {
        profile_id: String,
    },
    /// The assigned model is not configured, installed, or running.
    ModelUnavailable {
        model_id: String,
    },
    /// The model lacks a capability the task requires.
    Unmet {
        requirement: Unmet,
    },
    /// The task's privacy class forbids where the model runs.
    OffDevice {
        privacy: PrivacyClass,
    },
    /// A local model serving a private task has no pinned revision.
    Unpinned {
        model_id: String,
    },
    /// The assignment's options are not accepted by the model.
    Options {
        error: OptionError,
    },
    /// This device has an accelerator that becomes usable after a restart;
    /// the default layer waits for it rather than degrade.
    RestartRequired {
        vendor: AcceleratorVendor,
    },
    /// Nothing configured can serve the task by default.
    NoModel {
        privacy: PrivacyClass,
    },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTask => formatter.write_str("unknown model task"),
            Self::NotConsumer => formatter.write_str("caller may not request this model task"),
            Self::ProfileUnavailable { .. } => {
                formatter.write_str("the assigned provider is removed or disabled")
            }
            Self::ModelUnavailable { model_id } => {
                write!(formatter, "the assigned model {model_id} is not available")
            }
            Self::Unmet { requirement } => {
                write!(
                    formatter,
                    "the assigned model lacks a required capability ({requirement:?})"
                )
            }
            Self::OffDevice { .. } => {
                formatter.write_str("this task may only run on a model on this device")
            }
            Self::Unpinned { .. } => {
                formatter.write_str("the local model for this task has no pinned revision")
            }
            Self::Options { error } => write!(formatter, "the model does not accept {error:?}"),
            Self::RestartRequired { vendor } => write!(
                formatter,
                "restart to finish setting up the {} graphics driver",
                vendor.label()
            ),
            Self::NoModel { .. } => formatter.write_str("no model is set up for this task"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolve `task_id` for `consumer` (and `agent`, when the caller acts as
/// one) to a concrete profile and model.
///
/// # Errors
///
/// Returns [`ResolveError`] when the task is unknown, the consumer is not
/// admitted, or no layer can serve the task within its constraints.
pub fn resolve(
    inputs: &Inputs<'_>,
    consumer: &Consumer,
    task_id: &str,
    agent: Option<&str>,
) -> Result<Resolution, ResolveError> {
    resolve_for(inputs, consumer, task_id, agent, PrivacyClass::Open)
}

/// [`resolve`] for a request whose content is at least as private as
/// `content`. The request can raise the task's class, never lower it: a
/// triage of public reports stays [`PrivacyClass::Open`], the same task over
/// a private conversation resolves as [`PrivacyClass::Sensitive`] and so on
/// this device unless the person allowed otherwise.
///
/// # Errors
///
/// As [`resolve`].
pub fn resolve_for(
    inputs: &Inputs<'_>,
    consumer: &Consumer,
    task_id: &str,
    agent: Option<&str>,
    content: PrivacyClass,
) -> Result<Resolution, ResolveError> {
    let task = tasks::task(task_id).ok_or(ResolveError::UnknownTask)?;
    if !task.admits(consumer) {
        return Err(ResolveError::NotConsumer);
    }
    resolve_task_for(inputs, task, agent, content)
}

/// [`resolve`] without the consumer check, for Settings to show what each
/// task and agent resolves to.
///
/// # Errors
///
/// Returns [`ResolveError`] when no layer can serve the task.
pub fn resolve_task(
    inputs: &Inputs<'_>,
    task: &'static TaskSpec,
    agent: Option<&str>,
) -> Result<Resolution, ResolveError> {
    resolve_task_for(inputs, task, agent, PrivacyClass::Open)
}

/// [`resolve_task`] for content at least as private as `content`.
///
/// # Errors
///
/// Returns [`ResolveError`] when no layer can serve the task.
pub fn resolve_task_for(
    inputs: &Inputs<'_>,
    task: &'static TaskSpec,
    agent: Option<&str>,
    content: PrivacyClass,
) -> Result<Resolution, ResolveError> {
    let privacy = task.privacy.max(content);
    if task.per_agent
        && let Some(assignment) = agent.and_then(|agent| inputs.assignments.agent(agent, task.id))
    {
        return honour(inputs.config, task, privacy, assignment, Layer::Agent);
    }
    if let Some(assignment) = inputs.assignments.tasks.get(task.id) {
        return honour(inputs.config, task, privacy, assignment, Layer::Task);
    }
    let (assignment, layer) = match inputs.assignments.preset {
        Some(preset) => (
            preset_for(inputs, preset, task, privacy)?,
            Layer::Preset(preset),
        ),
        None => (recommended_for(inputs, task, privacy)?, Layer::Recommended),
    };
    honour(inputs.config, task, privacy, &assignment, layer)
}

/// Check an explicit assignment before Settings writes it, returning the
/// profile and model it would resolve to on this device.
///
/// # Errors
///
/// Returns [`ResolveError`] when the target is unavailable or breaks the
/// task's constraints.
pub fn check_assignment(
    config: &ProviderConfig,
    task: &'static TaskSpec,
    assignment: &Assignment,
) -> Result<Resolution, ResolveError> {
    honour(config, task, task.privacy, assignment, Layer::Task)
}

fn honour(
    config: &ProviderConfig,
    task: &'static TaskSpec,
    privacy: PrivacyClass,
    assignment: &Assignment,
    layer: Layer,
) -> Result<Resolution, ResolveError> {
    let (profile, model) = target(config, &assignment.target)?;
    task.requires
        .check(model)
        .map_err(|requirement| ResolveError::Unmet { requirement })?;
    // Where the model *actually* runs: only the model manager's supervised
    // runtime, which has no network, counts as this device. Anything else
    // that claims this device could forward what it is sent.
    let location = if profile.is_supervised() {
        EndpointLocation::ThisDevice
    } else if profile.location == EndpointLocation::ThisDevice {
        EndpointLocation::PrivateNetwork
    } else {
        profile.location
    };
    if !privacy.permits(location, assignment.allow_off_device) {
        return Err(ResolveError::OffDevice { privacy });
    }
    if privacy != PrivacyClass::Open
        && location == EndpointLocation::ThisDevice
        && !model.revision.as_deref().is_some_and(is_pinned_revision)
    {
        return Err(ResolveError::Unpinned {
            model_id: model.id.clone(),
        });
    }
    assignment
        .options
        .validate(profile.protocol, &model.options)
        .map_err(|error| ResolveError::Options { error })?;
    Ok(Resolution {
        task,
        privacy,
        profile: profile.clone(),
        model: model.clone(),
        options: assignment.options,
        layer,
    })
}

fn target<'a>(
    config: &'a ProviderConfig,
    target: &ModelTarget,
) -> Result<(&'a ProviderProfile, &'a ModelDescriptor), ResolveError> {
    let (profile, model_id) = match target {
        ModelTarget::Profile {
            profile_id,
            model_id,
        } => (
            config
                .profiles
                .iter()
                .find(|profile| {
                    &profile.id == profile_id && profile.enabled && !profile.is_model_manager()
                })
                .ok_or_else(|| ResolveError::ProfileUnavailable {
                    profile_id: profile_id.clone(),
                })?,
            model_id,
        ),
        ModelTarget::Local { model_id } => (
            config
                .local_profile()
                .filter(|profile| profile.enabled)
                .ok_or_else(|| ResolveError::ModelUnavailable {
                    model_id: model_id.clone(),
                })?,
            model_id,
        ),
    };
    // A record that fails validation (an HTTP endpoint for a hosted
    // provider, a malformed credential) is shown in Settings but never used.
    if config
        .validation_errors()
        .iter()
        .any(|issue| issue.profile_id == profile.id)
    {
        return Err(ResolveError::ProfileUnavailable {
            profile_id: profile.id.clone(),
        });
    }
    let model = profile
        .model(model_id)
        .ok_or_else(|| ResolveError::ModelUnavailable {
            model_id: model_id.clone(),
        })?;
    Ok((profile, model))
}

/// The best running local model for `task`, or why there is none.
fn local_choice(
    inputs: &Inputs<'_>,
    task: &TaskSpec,
    privacy: PrivacyClass,
) -> Result<Assignment, ResolveError> {
    let running = inputs
        .config
        .local_profile()
        .filter(|profile| profile.enabled)
        .into_iter()
        .flat_map(|profile| profile.models.iter())
        .filter(|model| {
            // The face runtime speaks only its fixed biometric protocol; it
            // serves face tasks and nothing else.
            model.declares(ModelCapability::FaceRecognition)
                == task
                    .requires
                    .capabilities
                    .contains(&ModelCapability::FaceRecognition)
                && task.requires.check(model).is_ok()
                && model.revision.as_deref().is_some_and(is_pinned_revision)
        })
        .max_by_key(|model| model.context_window.unwrap_or(0));
    if let Some(model) = running {
        return Ok(Assignment::local(&model.id));
    }
    if let Some(AcceleratorState::RestartRequired { vendor, .. }) =
        inputs.hardware.map(HardwareSnapshot::accelerator_state)
    {
        return Err(ResolveError::RestartRequired { vendor });
    }
    Err(ResolveError::NoModel { privacy })
}

/// The person's best provider model for `task`: the default provider first,
/// then any enabled provider, each by its default model first.
fn provider_choice(inputs: &Inputs<'_>, task: &TaskSpec) -> Option<Assignment> {
    let config = inputs.config;
    let mut profiles: Vec<&ProviderProfile> = config
        .profiles
        .iter()
        .filter(|profile| profile.enabled && !profile.is_model_manager())
        .collect();
    profiles.sort_by_key(|profile| Some(&profile.id) != config.default_profile.as_ref());
    profiles.into_iter().find_map(|profile| {
        let preferred = profile
            .default_model
            .as_deref()
            .and_then(|id| profile.model(id));
        preferred
            .into_iter()
            .chain(profile.models.iter())
            .find(|model| task.requires.check(model).is_ok())
            .map(|model| Assignment::profile(&profile.id, &model.id))
    })
}

fn strong_accelerator(hardware: Option<&HardwareSnapshot>) -> bool {
    matches!(
        hardware.map(HardwareSnapshot::accelerator_state),
        Some(AcceleratorState::Usable { vram_mib, .. }) if vram_mib >= STRONG_ACCELERATOR_MIB
    )
}

fn restart_pending(hardware: Option<&HardwareSnapshot>) -> Option<AcceleratorVendor> {
    match hardware.map(HardwareSnapshot::accelerator_state) {
        Some(AcceleratorState::RestartRequired { vendor, .. }) => Some(vendor),
        _ => None,
    }
}

/// What `preset` assigns `task` on this device.
///
/// # Errors
///
/// Returns [`ResolveError`] when the preset has nothing to serve the task
/// with here, including [`ResolveError::RestartRequired`] when the choice
/// waits for an accelerator.
pub fn preset_assignment(
    inputs: &Inputs<'_>,
    preset: Preset,
    task: &TaskSpec,
) -> Result<Assignment, ResolveError> {
    preset_for(inputs, preset, task, task.privacy)
}

fn preset_for(
    inputs: &Inputs<'_>,
    preset: Preset,
    task: &TaskSpec,
    privacy: PrivacyClass,
) -> Result<Assignment, ResolveError> {
    if privacy != PrivacyClass::Open || preset == Preset::Private {
        return local_choice(inputs, task, privacy);
    }
    match preset {
        Preset::Private => local_choice(inputs, task, privacy),
        Preset::Balanced => {
            if strong_accelerator(inputs.hardware)
                && let Ok(local) = local_choice(inputs, task, privacy)
            {
                return Ok(local);
            }
            // A strong-enough card waiting for its driver is not a reason to
            // move the task to a provider as if the device had none.
            if let Some(vendor) = restart_pending(inputs.hardware) {
                return Err(ResolveError::RestartRequired { vendor });
            }
            provider_choice(inputs, task).map_or_else(|| local_choice(inputs, task, privacy), Ok)
        }
        Preset::BestQuality => match provider_choice(inputs, task) {
            Some(mut assignment) => {
                assignment.options.reasoning_effort = highest_effort(inputs.config, &assignment);
                Ok(assignment)
            }
            None => local_choice(inputs, task, privacy),
        },
    }
}

/// The deepest effort up to `High` the assigned model accepts.
fn highest_effort(config: &ProviderConfig, assignment: &Assignment) -> Option<ReasoningEffort> {
    let (profile, model) = target(config, &assignment.target).ok()?;
    if !model.options.reasoning {
        return None;
    }
    ReasoningEffort::for_protocol(profile.protocol)
        .iter()
        .copied()
        .filter(|effort| *effort <= ReasoningEffort::High)
        .max()
}

/// The task's own recommendation when no preset is selected: open tasks on
/// the person's default provider, private tasks on this device.
///
/// # Errors
///
/// Returns [`ResolveError`] when nothing configured can serve the task.
pub fn recommended_assignment(
    inputs: &Inputs<'_>,
    task: &TaskSpec,
) -> Result<Assignment, ResolveError> {
    recommended_for(inputs, task, task.privacy)
}

fn recommended_for(
    inputs: &Inputs<'_>,
    task: &TaskSpec,
    privacy: PrivacyClass,
) -> Result<Assignment, ResolveError> {
    if privacy == PrivacyClass::Open
        && let Some(assignment) = provider_choice(inputs, task)
    {
        return Ok(assignment);
    }
    local_choice(inputs, task, privacy)
}

/// One task's outcome under a preset, for the preset picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetOutcome {
    pub task: &'static TaskSpec,
    pub outcome: Result<Resolution, ResolveError>,
}

/// What `preset` would do for every registered task on this device.
#[must_use]
pub fn preset_plan(inputs: &Inputs<'_>, preset: Preset) -> Vec<PresetOutcome> {
    tasks::REGISTRY
        .iter()
        .copied()
        .map(|task| PresetOutcome {
            task,
            outcome: preset_assignment(inputs, preset, task).and_then(|assignment| {
                honour(
                    inputs.config,
                    task,
                    task.privacy,
                    &assignment,
                    Layer::Preset(preset),
                )
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{Accelerator, RestartReason};
    use crate::tasks::{AGENT_PLANNING, FEEDBACK_REVIEW, PHOTOS_FACES, PHOTOS_VISION};
    use crate::{Credential, ModelOptionSupport, ProviderProtocol};

    const PIN: &str = "0123456789abcdef0123456789abcdef01234567";

    fn local_model(id: &str, capabilities: &[ModelCapability]) -> ModelDescriptor {
        ModelDescriptor {
            context_window: Some(32_768),
            revision: Some(PIN.into()),
            capabilities: capabilities.iter().copied().collect(),
            ..ModelDescriptor::new(id)
        }
    }

    fn azure() -> ProviderProfile {
        let mut profile = ProviderProfile::hosted(ProviderProtocol::AzureOpenAi);
        profile.id = "azure".into();
        profile.credential = Some(Credential::Account {
            account_id: "entra".into(),
        });
        profile.models.push(ModelDescriptor {
            context_window: Some(400_000),
            options: ModelOptionSupport {
                fast_mode: true,
                reasoning: true,
            },
            ..ModelDescriptor::new("gpt-5.6-sol")
        });
        profile.default_model = Some("gpt-5.6-sol".into());
        profile
    }

    fn anthropic() -> ProviderProfile {
        let mut profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        profile.id = "anthropic".into();
        profile.models.push(ModelDescriptor {
            context_window: Some(1_000_000),
            ..ModelDescriptor::new("claude-opus-5")
        });
        profile.default_model = Some("claude-opus-5".into());
        profile
    }

    fn device(running: bool) -> ProviderProfile {
        let mut profile = ProviderProfile::local_runtime(
            running.then(|| "/run/user/1000/tilo/models/a/api.sock".into()),
        );
        profile.models = vec![
            local_model("qwen3-4b", &[ModelCapability::TraceReview]),
            local_model("qwen-vl", &[ModelCapability::Vision]),
            local_model("faces", &[ModelCapability::FaceRecognition]),
        ];
        profile
    }

    fn config() -> ProviderConfig {
        ProviderConfig {
            profiles: vec![anthropic(), azure(), device(true)],
            default_profile: Some("anthropic".into()),
        }
    }

    fn card(vram_mib: u64) -> Accelerator {
        Accelerator {
            vendor: AcceleratorVendor::Nvidia,
            name: "NVIDIA GeForce RTX 4090".into(),
            pci_slot: "0000:01:00.0".into(),
            render_node: Some("renderD128".into()),
            kernel_driver: "nvidia".into(),
            integrated: false,
            dedicated_mib: vram_mib,
            shared_mib: 0,
            compute: Some("580".into()),
            restart: None,
        }
    }

    fn gpu(vram_mib: u64) -> HardwareSnapshot {
        HardwareSnapshot {
            cpu_threads: 16,
            memory_mib: 32_768,
            available_memory_mib: 24_000,
            free_storage_mib: 100_000,
            accelerators: vec![card(vram_mib)],
        }
    }

    fn inputs<'a>(
        config: &'a ProviderConfig,
        assignments: &'a ModelAssignments,
        hardware: Option<&'a HardwareSnapshot>,
    ) -> Inputs<'a> {
        Inputs {
            config,
            assignments,
            hardware,
        }
    }

    fn fast_azure() -> Assignment {
        Assignment {
            options: ModelOptions {
                fast_mode: true,
                reasoning_effort: None,
            },
            ..Assignment::profile("azure", "gpt-5.6-sol")
        }
    }

    #[test]
    fn the_agent_layer_wins_over_the_task_which_wins_over_the_default() {
        let config = config();
        let mut assignments = ModelAssignments::default();
        let agent = Consumer::app(tasks::AGENT_APP);
        let default = resolve(
            &inputs(&config, &assignments, None),
            &agent,
            AGENT_PLANNING.id,
            Some("X"),
        )
        .unwrap();
        assert_eq!(default.layer, Layer::Recommended);
        assert_eq!(default.profile.id, "anthropic");

        assignments
            .tasks
            .insert(AGENT_PLANNING.id.into(), Assignment::local("qwen3-4b"));
        let task = resolve(
            &inputs(&config, &assignments, None),
            &agent,
            AGENT_PLANNING.id,
            Some("X"),
        )
        .unwrap();
        assert_eq!(
            (task.layer, task.model.id.as_str()),
            (Layer::Task, "qwen3-4b")
        );

        assignments.set_agent("X", AGENT_PLANNING.id, fast_azure());
        let resolved = resolve(
            &inputs(&config, &assignments, None),
            &agent,
            AGENT_PLANNING.id,
            Some("X"),
        )
        .unwrap();
        assert_eq!(resolved.layer, Layer::Agent);
        assert_eq!(resolved.model.id, "gpt-5.6-sol");
        assert!(resolved.options.fast_mode);
        // Another agent keeps the task assignment.
        let other = resolve(
            &inputs(&config, &assignments, None),
            &agent,
            AGENT_PLANNING.id,
            Some("Y"),
        )
        .unwrap();
        assert_eq!(other.layer, Layer::Task);
    }

    #[test]
    fn a_device_only_task_refuses_a_hosted_assignment_whatever_the_layer() {
        let config = config();
        let mut assignments = ModelAssignments::default();
        let mut hosted = Assignment::profile("anthropic", "claude-opus-5");
        hosted.allow_off_device = true;
        // The hosted model does not even declare vision; give it the
        // capability so only the privacy class is in the way.
        let mut config_with_vision = config;
        config_with_vision.profiles[0].models[0]
            .capabilities
            .insert(ModelCapability::Vision);
        assignments.tasks.insert(PHOTOS_VISION.id.into(), hosted);
        let photos = Consumer::app(tasks::PHOTOS_APP);
        assert_eq!(
            resolve(
                &inputs(&config_with_vision, &assignments, None),
                &photos,
                PHOTOS_VISION.id,
                None
            ),
            Err(ResolveError::OffDevice {
                privacy: PrivacyClass::DeviceOnly
            })
        );
        assert!(
            check_assignment(
                &config_with_vision,
                &PHOTOS_VISION,
                &assignments.tasks[PHOTOS_VISION.id]
            )
            .is_err()
        );
    }

    #[test]
    fn a_sensitive_task_stays_local_until_the_person_allows_otherwise() {
        let mut config = config();
        // The person claims the hosted model speaks the review protocol.
        config.profiles[0].models[0]
            .capabilities
            .insert(ModelCapability::TraceReview);
        let mut assignments = ModelAssignments::default();
        let session = Consumer::Session;
        let default = resolve(
            &inputs(&config, &assignments, None),
            &session,
            FEEDBACK_REVIEW.id,
            None,
        )
        .unwrap();
        assert!(default.profile.is_device_local());

        let mut hosted = Assignment::profile("anthropic", "claude-opus-5");
        assignments
            .tasks
            .insert(FEEDBACK_REVIEW.id.into(), hosted.clone());
        assert_eq!(
            resolve(
                &inputs(&config, &assignments, None),
                &session,
                FEEDBACK_REVIEW.id,
                None
            ),
            Err(ResolveError::OffDevice {
                privacy: PrivacyClass::Sensitive
            })
        );
        hosted.allow_off_device = true;
        assignments.tasks.insert(FEEDBACK_REVIEW.id.into(), hosted);
        let allowed = resolve(
            &inputs(&config, &assignments, None),
            &session,
            FEEDBACK_REVIEW.id,
            None,
        )
        .unwrap();
        assert_eq!(allowed.profile.id, "anthropic");
        // Presets never grant the allowance.
        let preset = ModelAssignments {
            preset: Some(Preset::BestQuality),
            ..ModelAssignments::default()
        };
        let best = resolve(
            &inputs(&config, &preset, None),
            &session,
            FEEDBACK_REVIEW.id,
            None,
        )
        .unwrap();
        assert!(best.profile.is_device_local());
    }

    #[test]
    fn feedback_triage_resolves_by_the_privacy_of_the_report_it_reads() {
        use crate::tasks::FEEDBACK_TRIAGE;
        let config = config();
        let agent = Consumer::app(tasks::AGENT_APP);
        let mut assignments = ModelAssignments::default();
        let triage = |assignments: &ModelAssignments, content| {
            resolve_for(
                &inputs(&config, assignments, None),
                &agent,
                FEEDBACK_TRIAGE.id,
                Some("triage"),
                content,
            )
        };

        // A public #feedback report is open content: the default provider.
        let public = triage(&assignments, PrivacyClass::Open).unwrap();
        assert_eq!(
            (public.profile.id.as_str(), public.privacy),
            ("anthropic", PrivacyClass::Open)
        );
        // A report from a direct room raises the class: this device.
        let private = triage(&assignments, PrivacyClass::Sensitive).unwrap();
        assert!(private.profile.is_device_local());
        assert_eq!(private.privacy, PrivacyClass::Sensitive);

        // A hosted assignment serves the public reports and refuses the
        // private ones until the person allows it.
        let mut hosted = Assignment::profile("anthropic", "claude-opus-5");
        assignments.set_agent("triage", FEEDBACK_TRIAGE.id, hosted.clone());
        assert_eq!(
            triage(&assignments, PrivacyClass::Open).unwrap().layer,
            Layer::Agent
        );
        assert_eq!(
            triage(&assignments, PrivacyClass::Sensitive),
            Err(ResolveError::OffDevice {
                privacy: PrivacyClass::Sensitive
            })
        );
        hosted.allow_off_device = true;
        assignments.set_agent("triage", FEEDBACK_TRIAGE.id, hosted);
        assert_eq!(
            triage(&assignments, PrivacyClass::Sensitive)
                .unwrap()
                .profile
                .id,
            "anthropic"
        );

        // A request can raise a class but never lower one: the review task
        // stays sensitive whatever its caller claims.
        assert_eq!(
            resolve_for(
                &inputs(&config, &ModelAssignments::default(), None),
                &Consumer::Session,
                FEEDBACK_REVIEW.id,
                None,
                PrivacyClass::Open,
            )
            .unwrap()
            .privacy,
            PrivacyClass::Sensitive
        );
        // Only the agent consumes it.
        assert_eq!(
            resolve_for(
                &inputs(&config, &assignments, None),
                &Consumer::Session,
                FEEDBACK_TRIAGE.id,
                None,
                PrivacyClass::Open,
            ),
            Err(ResolveError::NotConsumer)
        );
    }

    #[test]
    fn a_server_the_person_runs_on_this_computer_counts_as_off_device() {
        let mut ollama = ProviderProfile::new(
            ProviderProtocol::OpenAiCompatible,
            EndpointLocation::ThisDevice,
        );
        ollama.id = "ollama".into();
        ollama.models.push(ModelDescriptor {
            context_window: Some(32_768),
            capabilities: [ModelCapability::TraceReview, ModelCapability::Vision].into(),
            ..ModelDescriptor::new("llama3")
        });
        let config = ProviderConfig {
            profiles: vec![ollama],
            default_profile: Some("ollama".into()),
        };
        let mut assignments = ModelAssignments::default();
        // Any provider may serve an open task.
        let planning = resolve(
            &inputs(&config, &assignments, None),
            &Consumer::app(tasks::AGENT_APP),
            AGENT_PLANNING.id,
            None,
        )
        .unwrap();
        assert_eq!(planning.profile.id, "ollama");
        // It is not isolated by Tilo, so a sensitive task needs the explicit
        // allowance and a device-only task never uses it.
        let mut review = Assignment::profile("ollama", "llama3");
        assignments
            .tasks
            .insert(FEEDBACK_REVIEW.id.into(), review.clone());
        assert_eq!(
            resolve(
                &inputs(&config, &assignments, None),
                &Consumer::Session,
                FEEDBACK_REVIEW.id,
                None
            ),
            Err(ResolveError::OffDevice {
                privacy: PrivacyClass::Sensitive
            })
        );
        review.allow_off_device = true;
        assignments.tasks.insert(FEEDBACK_REVIEW.id.into(), review);
        assert!(
            !resolve(
                &inputs(&config, &assignments, None),
                &Consumer::Session,
                FEEDBACK_REVIEW.id,
                None
            )
            .unwrap()
            .profile
            .is_supervised()
        );
        let mut vision = Assignment::profile("ollama", "llama3");
        vision.allow_off_device = true;
        assignments.tasks.insert(PHOTOS_VISION.id.into(), vision);
        assert_eq!(
            resolve(
                &inputs(&config, &assignments, None),
                &Consumer::app(tasks::PHOTOS_APP),
                PHOTOS_VISION.id,
                None
            ),
            Err(ResolveError::OffDevice {
                privacy: PrivacyClass::DeviceOnly
            })
        );
    }

    #[test]
    fn a_caller_can_ask_only_for_tasks_it_consumes() {
        let config = config();
        let assignments = ModelAssignments::default();
        assert_eq!(
            resolve(
                &inputs(&config, &assignments, None),
                &Consumer::app(tasks::PHOTOS_APP),
                AGENT_PLANNING.id,
                None
            ),
            Err(ResolveError::NotConsumer)
        );
        assert_eq!(
            resolve(
                &inputs(&config, &assignments, None),
                &Consumer::Session,
                "made.up",
                None
            ),
            Err(ResolveError::UnknownTask)
        );
    }

    #[test]
    fn an_unavailable_explicit_choice_is_reported_not_silently_replaced() {
        let mut config = config();
        let mut assignments = ModelAssignments::default();
        assignments
            .tasks
            .insert(AGENT_PLANNING.id.into(), fast_azure());
        config.profiles[1].enabled = false;
        assert_eq!(
            resolve(
                &inputs(&config, &assignments, None),
                &Consumer::app(tasks::AGENT_APP),
                AGENT_PLANNING.id,
                None
            ),
            Err(ResolveError::ProfileUnavailable {
                profile_id: "azure".into()
            })
        );
        // Options the model does not declare are refused.
        let mut effort = Assignment::profile("anthropic", "claude-opus-5");
        effort.options.reasoning_effort = Some(ReasoningEffort::High);
        assert_eq!(
            check_assignment(&config, &AGENT_PLANNING, &effort),
            Err(ResolveError::Options {
                error: OptionError::ReasoningUnsupported
            })
        );
    }

    #[test]
    fn presets_adapt_to_the_hardware_and_the_configured_providers() {
        let config = config();
        let balanced = ModelAssignments {
            preset: Some(Preset::Balanced),
            ..ModelAssignments::default()
        };
        let agent = Consumer::app(tasks::AGENT_APP);
        // A 24 GiB card runs open tasks locally under Balanced.
        let strong = gpu(24_576);
        let local = resolve(
            &inputs(&config, &balanced, Some(&strong)),
            &agent,
            AGENT_PLANNING.id,
            None,
        )
        .unwrap();
        assert!(local.profile.is_device_local());
        assert_eq!(local.layer, Layer::Preset(Preset::Balanced));
        // An 8 GiB card sends them to the default provider instead.
        let modest = gpu(8_188);
        let hosted = resolve(
            &inputs(&config, &balanced, Some(&modest)),
            &agent,
            AGENT_PLANNING.id,
            None,
        )
        .unwrap();
        assert_eq!(hosted.profile.id, "anthropic");
        // Private never leaves the device; with no provider configured,
        // Balanced falls back to the device too.
        let private = ModelAssignments {
            preset: Some(Preset::Private),
            ..ModelAssignments::default()
        };
        assert!(
            resolve(
                &inputs(&config, &private, Some(&modest)),
                &agent,
                AGENT_PLANNING.id,
                None
            )
            .unwrap()
            .profile
            .is_device_local()
        );
        let only_local = ProviderConfig {
            profiles: vec![device(true)],
            default_profile: None,
        };
        assert!(
            resolve(
                &inputs(&only_local, &balanced, Some(&modest)),
                &agent,
                AGENT_PLANNING.id,
                None
            )
            .unwrap()
            .profile
            .is_device_local()
        );
        // Best quality raises the effort where the model takes one.
        let best = ModelAssignments {
            preset: Some(Preset::BestQuality),
            ..ModelAssignments::default()
        };
        let azure_default = ProviderConfig {
            default_profile: Some("azure".into()),
            ..config.clone()
        };
        let deep = resolve(
            &inputs(&azure_default, &best, None),
            &agent,
            AGENT_PLANNING.id,
            None,
        )
        .unwrap();
        assert_eq!(deep.options.reasoning_effort, Some(ReasoningEffort::High));
        // Every preset keeps faces on the device.
        for preset in Preset::ALL {
            let plan = preset_plan(
                &inputs(&config, &ModelAssignments::default(), Some(&strong)),
                preset,
            );
            let faces = plan
                .iter()
                .find(|outcome| outcome.task.id == PHOTOS_FACES.id)
                .unwrap();
            assert_eq!(faces.outcome.as_ref().unwrap().model.id, "faces");
        }
    }

    #[test]
    fn a_card_waiting_for_a_restart_holds_presets_instead_of_degrading() {
        let pending = HardwareSnapshot {
            accelerators: vec![Accelerator {
                compute: None,
                restart: Some(RestartReason::DriverStaged),
                ..card(0)
            }],
            ..gpu(0)
        };
        // Nothing runs locally yet: the card is waiting for its driver.
        let config = ProviderConfig {
            profiles: vec![anthropic(), device(false)],
            default_profile: Some("anthropic".into()),
        };
        let balanced = ModelAssignments {
            preset: Some(Preset::Balanced),
            ..ModelAssignments::default()
        };
        let restart = Err(ResolveError::RestartRequired {
            vendor: AcceleratorVendor::Nvidia,
        });
        let agent = Consumer::app(tasks::AGENT_APP);
        assert_eq!(
            resolve(
                &inputs(&config, &balanced, Some(&pending)),
                &agent,
                AGENT_PLANNING.id,
                None
            ),
            restart
        );
        assert_eq!(
            resolve(
                &inputs(&config, &balanced, Some(&pending)),
                &Consumer::Session,
                FEEDBACK_REVIEW.id,
                None
            ),
            restart
        );
        // With no accelerator at all the same device reports plainly that no
        // local model is set up, and Balanced uses the provider.
        let absent = HardwareSnapshot {
            accelerators: Vec::new(),
            ..gpu(0)
        };
        assert_eq!(
            resolve(
                &inputs(&config, &balanced, Some(&absent)),
                &agent,
                AGENT_PLANNING.id,
                None
            )
            .unwrap()
            .profile
            .id,
            "anthropic"
        );
    }

    #[test]
    fn a_stopped_or_unpinned_local_model_does_not_serve_private_tasks() {
        let assignments = ModelAssignments::default();
        let stopped = ProviderConfig {
            profiles: vec![device(false)],
            default_profile: None,
        };
        assert_eq!(
            resolve(
                &inputs(&stopped, &assignments, None),
                &Consumer::app(tasks::PHOTOS_APP),
                PHOTOS_VISION.id,
                None
            ),
            Err(ResolveError::NoModel {
                privacy: PrivacyClass::DeviceOnly
            })
        );
        let mut unpinned = device(true);
        for model in &mut unpinned.models {
            model.revision = Some("main".into());
        }
        let config = ProviderConfig {
            profiles: vec![unpinned],
            default_profile: None,
        };
        let mut explicit = ModelAssignments::default();
        explicit
            .tasks
            .insert(PHOTOS_VISION.id.into(), Assignment::local("qwen-vl"));
        assert_eq!(
            resolve(
                &inputs(&config, &explicit, None),
                &Consumer::app(tasks::PHOTOS_APP),
                PHOTOS_VISION.id,
                None
            ),
            Err(ResolveError::Unpinned {
                model_id: "qwen-vl".into()
            })
        );
    }

    #[test]
    fn assignments_persist_in_their_own_personal_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut assignments = ModelAssignments {
            preset: Some(Preset::Balanced),
            ..ModelAssignments::default()
        };
        assignments
            .tasks
            .insert(FEEDBACK_REVIEW.id.into(), Assignment::local("qwen3-4b"));
        assignments.set_agent("X", AGENT_PLANNING.id, fast_azure());
        // Not per-agent: dropped on load.
        assignments.set_agent("X", PHOTOS_FACES.id, Assignment::local("faces"));
        assignments.save_at(&path).unwrap();
        config().save_at(&path).unwrap();

        let loaded = ModelAssignments::load_at(&path);
        assert_eq!(loaded.preset, Some(Preset::Balanced));
        assert_eq!(loaded.agent("X", AGENT_PLANNING.id), Some(&fast_azure()));
        assert!(loaded.agent("X", PHOTOS_FACES.id).is_none());
        let document = settings_store::load_at(&path);
        let section = document[SETTINGS_SECTION].as_object().unwrap();
        assert!(section.contains_key(ASSIGNMENTS_KEY));
        for (leaf, scope) in [
            (
                crate::PROFILES_KEY,
                settings_store::continuity::SettingScope::Personal,
            ),
            (
                ASSIGNMENTS_KEY,
                settings_store::continuity::SettingScope::Personal,
            ),
            (
                crate::THIS_DEVICE_KEY,
                settings_store::continuity::SettingScope::DeviceLocal,
            ),
        ] {
            assert_eq!(
                settings_store::continuity::classify(&format!("tilo.{SETTINGS_SECTION}.{leaf}")),
                Some(scope),
                "{leaf}"
            );
        }
        let mut forgetting = loaded;
        forgetting.forget_profile("azure");
        assert!(forgetting.agents.is_empty());
    }
}
