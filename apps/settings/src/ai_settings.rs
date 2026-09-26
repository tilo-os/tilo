//! The typed operations behind the AI hub, and the one function that applies
//! them to the person's provider profiles and assignments.
//!
//! Every control in the AI pane and every agent or `tilo-control` caller
//! reaches the same [`AiOperation`] through the resource tree
//! (`crate::actions::ai_section`). Operations that touch the vault or open a
//! browser return a [`SideEffect`] the host performs; everything else is a
//! pure edit of [`ProviderConfig`] and [`ModelAssignments`], validated with
//! the gateway's own resolver before it is written.

use std::fmt;

use agent_provider_config::assign::{self, Assignment, ModelAssignments, ModelTarget, Preset};
use agent_provider_config::manager::LocalModelSnapshot;
use agent_provider_config::tasks::{self, PrivacyClass, TaskSpec};
use agent_provider_config::{
    Credential, EndpointLocation, ModelCapability, ModelDescriptor, ProviderConfig,
    ProviderProfile, ProviderProtocol, ReasoningEffort, SecretReference,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Registry type id of every immediate AI hub operation.
pub const AI_OPERATION_TYPE: &str = "settings.ai-operation";

/// Where an assignment applies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum AssignScope {
    Task { task: String },
    Agent { agent: String, task: String },
}

impl AssignScope {
    fn task(&self) -> Result<&'static TaskSpec, String> {
        let id = match self {
            Self::Task { task } | Self::Agent { task, .. } => task,
        };
        let spec = tasks::task(id).ok_or_else(|| format!("unknown model task {id}"))?;
        if matches!(self, Self::Agent { .. }) && !spec.per_agent {
            return Err("this task has no per-agent assignment".into());
        }
        Ok(spec)
    }
}

/// A declared, non-capability model feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFeature {
    Tools,
    FastMode,
    Reasoning,
}

/// One change to the AI hub's durable state.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AiOperation {
    AddProvider {
        protocol: ProviderProtocol,
        location: EndpointLocation,
    },
    RemoveProvider {
        id: String,
    },
    SetProviderEnabled {
        id: String,
        enabled: bool,
    },
    SetDefaultProvider {
        id: String,
    },
    SetProviderDetails {
        id: String,
        label: String,
        endpoint: String,
    },
    AddModel {
        profile: String,
        model: String,
        context_window: Option<u32>,
    },
    RemoveModel {
        profile: String,
        model: String,
    },
    SetModelCapability {
        profile: String,
        model: String,
        capability: ModelCapability,
        declared: bool,
    },
    SetModelFeature {
        profile: String,
        model: String,
        feature: ModelFeature,
        enabled: bool,
    },
    /// The key crosses only this process and the gateway; it is never
    /// written to a settings document.
    SetApiKey {
        profile: String,
        key: String,
    },
    RemoveApiKey {
        profile: String,
    },
    SignIn {
        profile: String,
    },
    SignOut {
        profile: String,
    },
    SetPreset {
        preset: Option<Preset>,
    },
    /// `None` returns the scope to its default (the preset or the task's
    /// recommendation for a task; the task's own assignment for an agent).
    ChooseModel {
        scope: AssignScope,
        target: Option<ModelTarget>,
    },
    /// The explicit allowance for a sensitive task to run off this device.
    AllowOffDevice {
        scope: AssignScope,
        target: ModelTarget,
    },
    SetFastMode {
        scope: AssignScope,
        enabled: bool,
    },
    SetReasoningEffort {
        scope: AssignScope,
        effort: Option<ReasoningEffort>,
    },
}

impl fmt::Debug for AiOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SetApiKey { profile, .. } => formatter
                .debug_struct("SetApiKey")
                .field("profile", profile)
                .field("key", &"<redacted>")
                .finish(),
            other => {
                let value = serde_json::to_value(other).unwrap_or_default();
                write!(formatter, "{value}")
            }
        }
    }
}

/// Work the host performs after a successful edit.
#[derive(Debug, PartialEq, Eq)]
pub enum SideEffect {
    None,
    StoreKey {
        profile: String,
        key: String,
    },
    RemoveKey {
        profile: String,
    },
    SignIn {
        profile: String,
    },
    /// Forget the account's sealed tokens.
    SignOut {
        account_id: String,
    },
}

impl Drop for SideEffect {
    fn drop(&mut self) {
        if let Self::StoreKey { key, .. } = self {
            key.zeroize();
        }
    }
}

/// Stable action-argument spelling of a target.
#[must_use]
pub fn target_value(target: &ModelTarget) -> String {
    match target {
        ModelTarget::Local { model_id } => format!("local:{model_id}"),
        ModelTarget::Profile {
            profile_id,
            model_id,
        } => format!("{profile_id}/{model_id}"),
    }
}

/// Parse [`target_value`] back.
#[must_use]
pub fn parse_target(value: &str) -> Option<ModelTarget> {
    if let Some(model_id) = value.strip_prefix("local:") {
        return (!model_id.is_empty()).then(|| ModelTarget::Local {
            model_id: model_id.to_string(),
        });
    }
    let (profile_id, model_id) = value.split_once('/')?;
    (!profile_id.is_empty() && !model_id.is_empty()).then(|| ModelTarget::Profile {
        profile_id: profile_id.to_string(),
        model_id: model_id.to_string(),
    })
}

fn profile_mut<'a>(
    config: &'a mut ProviderConfig,
    id: &str,
) -> Result<&'a mut ProviderProfile, String> {
    config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == id && !profile.is_device_local())
        .ok_or_else(|| "provider is no longer available".to_string())
}

fn model_mut<'a>(
    config: &'a mut ProviderConfig,
    profile: &str,
    model: &str,
) -> Result<&'a mut ModelDescriptor, String> {
    profile_mut(config, profile)?
        .models
        .iter_mut()
        .find(|candidate| candidate.id == model)
        .ok_or_else(|| "model is no longer configured".to_string())
}

fn scope_assignment<'a>(
    assignments: &'a mut ModelAssignments,
    scope: &AssignScope,
) -> Option<&'a mut Assignment> {
    match scope {
        AssignScope::Task { task } => assignments.tasks.get_mut(task),
        AssignScope::Agent { agent, task } => assignments.agents.get_mut(agent)?.get_mut(task),
    }
}

fn set_scope(
    assignments: &mut ModelAssignments,
    scope: &AssignScope,
    assignment: Option<Assignment>,
) {
    match (scope, assignment) {
        (AssignScope::Task { task }, Some(assignment)) => {
            assignments.tasks.insert(task.clone(), assignment);
        }
        (AssignScope::Task { task }, None) => {
            assignments.tasks.remove(task);
        }
        (AssignScope::Agent { agent, task }, Some(assignment)) => {
            assignments.set_agent(agent, task, assignment);
        }
        (AssignScope::Agent { agent, task }, None) => assignments.clear_agent(agent, task),
    }
}

/// Check `assignment` for `task` against the resolver the gateway uses, so
/// nothing is stored that the gateway would refuse.
fn checked(
    config: &ProviderConfig,
    task: &'static TaskSpec,
    assignment: Assignment,
) -> Result<Assignment, String> {
    assign::check_assignment(config, task, &assignment).map_err(|error| error.to_string())?;
    Ok(assignment)
}

/// Apply `operation` to the person's state. The caller persists `config` and
/// `assignments` and performs the returned [`SideEffect`].
///
/// # Errors
///
/// Returns a person-readable reason when the operation names something that
/// no longer exists or would break a task's constraints.
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive dispatch keeps every hub mutation auditable in one place"
)]
pub fn apply(
    operation: AiOperation,
    config: &mut ProviderConfig,
    assignments: &mut ModelAssignments,
) -> Result<SideEffect, String> {
    match operation {
        AiOperation::AddProvider { protocol, location } => {
            if location == EndpointLocation::ThisDevice {
                return Err("models on this device are managed by the local model service".into());
            }
            config.upsert(ProviderProfile::new(protocol, location));
            Ok(SideEffect::None)
        }
        AiOperation::RemoveProvider { id } => {
            let removed = profile_mut(config, &id)?.clone();
            let _ = config.remove(&id);
            assignments.forget_profile(&id);
            Ok(match removed.credential {
                Some(Credential::ApiKey { .. }) => SideEffect::RemoveKey { profile: id },
                Some(Credential::Account { account_id }) => SideEffect::SignOut { account_id },
                None => SideEffect::None,
            })
        }
        AiOperation::SetProviderEnabled { id, enabled } => {
            profile_mut(config, &id)?.enabled = enabled;
            Ok(SideEffect::None)
        }
        AiOperation::SetDefaultProvider { id } => {
            let profile = profile_mut(config, &id)?;
            if !profile.enabled || profile.models.is_empty() {
                return Err(
                    "add a model and enable this provider before making it the default".into(),
                );
            }
            config.default_profile = Some(id);
            Ok(SideEffect::None)
        }
        AiOperation::SetProviderDetails {
            id,
            label,
            endpoint,
        } => {
            if label.trim().is_empty() || endpoint.trim().is_empty() {
                return Err("provider name and endpoint are required".into());
            }
            let profile = profile_mut(config, &id)?;
            profile.label = label.trim().to_string();
            profile.endpoint = endpoint.trim().to_string();
            if let Some(issue) = config
                .validation_errors()
                .into_iter()
                .find(|issue| issue.profile_id == id)
            {
                return Err(issue.message);
            }
            Ok(SideEffect::None)
        }
        AiOperation::AddModel {
            profile,
            model,
            context_window,
        } => {
            let model = model.trim().to_string();
            if model.is_empty() || model.chars().any(char::is_control) || model.len() > 256 {
                return Err("enter the model id (for Azure, the deployment name)".into());
            }
            let entry = profile_mut(config, &profile)?;
            if entry.model(&model).is_some() {
                return Err("this model is already configured".into());
            }
            entry.models.push(ModelDescriptor {
                context_window: context_window.filter(|window| *window > 0),
                ..ModelDescriptor::new(model.clone())
            });
            if entry.default_model.is_none() {
                entry.default_model = Some(model);
            }
            Ok(SideEffect::None)
        }
        AiOperation::RemoveModel { profile, model } => {
            let entry = profile_mut(config, &profile)?;
            entry.models.retain(|candidate| candidate.id != model);
            if entry.default_model.as_deref() == Some(model.as_str()) {
                entry.default_model = entry.models.first().map(|first| first.id.clone());
            }
            let names = |assignment: &Assignment| {
                matches!(&assignment.target, ModelTarget::Profile { profile_id, model_id }
                    if *profile_id == profile && *model_id == model)
            };
            assignments.tasks.retain(|_, assignment| !names(assignment));
            for per_agent in assignments.agents.values_mut() {
                per_agent.retain(|_, assignment| !names(assignment));
            }
            assignments
                .agents
                .retain(|_, per_agent| !per_agent.is_empty());
            Ok(SideEffect::None)
        }
        AiOperation::SetModelCapability {
            profile,
            model,
            capability,
            declared,
        } => {
            // Face recognition is a signed local protocol; a hosted model
            // never claims it.
            if capability == ModelCapability::FaceRecognition && declared {
                return Err("face recognition runs only on a signed model on this device".into());
            }
            let entry = model_mut(config, &profile, &model)?;
            if declared {
                entry.capabilities.insert(capability);
            } else {
                entry.capabilities.remove(&capability);
            }
            Ok(SideEffect::None)
        }
        AiOperation::SetModelFeature {
            profile,
            model,
            feature,
            enabled,
        } => {
            let entry = model_mut(config, &profile, &model)?;
            match feature {
                ModelFeature::Tools => entry.tool_calling = enabled,
                ModelFeature::FastMode => entry.options.fast_mode = enabled,
                ModelFeature::Reasoning => entry.options.reasoning = enabled,
            }
            Ok(SideEffect::None)
        }
        AiOperation::SetApiKey { profile, mut key } => {
            let entry = profile_mut(config, &profile)?;
            if key.trim().is_empty() || key.len() > 16 * 1024 {
                key.zeroize();
                return Err("enter the API key".into());
            }
            entry.credential = Some(Credential::ApiKey {
                reference: SecretReference::provider_api_key(&profile),
            });
            Ok(SideEffect::StoreKey { profile, key })
        }
        AiOperation::RemoveApiKey { profile } => {
            let _ = profile_mut(config, &profile)?;
            Ok(SideEffect::RemoveKey { profile })
        }
        AiOperation::SignIn { profile } => {
            let entry = profile_mut(config, &profile)?;
            if !entry.protocol.supports_account_sign_in() {
                return Err("this provider takes an API key, not an account sign-in".into());
            }
            Ok(SideEffect::SignIn { profile })
        }
        AiOperation::SignOut { profile } => {
            let entry = profile_mut(config, &profile)?;
            let Some(Credential::Account { account_id }) = entry.credential.take() else {
                return Err("this provider is not signed in".into());
            };
            entry.credential = Some(Credential::ApiKey {
                reference: SecretReference::provider_api_key(&profile),
            });
            Ok(SideEffect::SignOut { account_id })
        }
        AiOperation::SetPreset { preset } => {
            assignments.preset = preset;
            Ok(SideEffect::None)
        }
        AiOperation::ChooseModel { scope, target } => {
            let task = scope.task()?;
            let Some(target) = target else {
                set_scope(assignments, &scope, None);
                return Ok(SideEffect::None);
            };
            let previous = scope_assignment(assignments, &scope).cloned();
            let mut assignment = Assignment {
                target,
                ..Assignment::local("")
            };
            // Keep options the new model also accepts; keep an allowance only
            // for the same off-device target.
            if let Some(previous) = previous {
                assignment.options = previous.options;
                assignment.allow_off_device =
                    previous.allow_off_device && previous.target == assignment.target;
            }
            let assignment = match checked(config, task, assignment.clone()) {
                Ok(assignment) => assignment,
                Err(_) if assignment.options != agent_provider_config::ModelOptions::default() => {
                    checked(
                        config,
                        task,
                        Assignment {
                            options: agent_provider_config::ModelOptions::default(),
                            ..assignment
                        },
                    )?
                }
                Err(error) => {
                    if task.privacy == PrivacyClass::Sensitive {
                        return Err(format!(
                            "{error}; allow this task to run off this device first"
                        ));
                    }
                    return Err(error);
                }
            };
            set_scope(assignments, &scope, Some(assignment));
            Ok(SideEffect::None)
        }
        AiOperation::AllowOffDevice { scope, target } => {
            let task = scope.task()?;
            if task.privacy != PrivacyClass::Sensitive {
                return Err(match task.privacy {
                    PrivacyClass::DeviceOnly => "this task never leaves this device".into(),
                    _ => "this task may already use any provider".into(),
                });
            }
            let assignment = checked(
                config,
                task,
                Assignment {
                    target,
                    options: agent_provider_config::ModelOptions::default(),
                    allow_off_device: true,
                },
            )?;
            set_scope(assignments, &scope, Some(assignment));
            Ok(SideEffect::None)
        }
        AiOperation::SetFastMode { scope, enabled } => {
            let task = scope.task()?;
            let mut assignment = scope_assignment(assignments, &scope)
                .cloned()
                .ok_or("choose a model for this first")?;
            assignment.options.fast_mode = enabled;
            let assignment = checked(config, task, assignment)?;
            set_scope(assignments, &scope, Some(assignment));
            Ok(SideEffect::None)
        }
        AiOperation::SetReasoningEffort { scope, effort } => {
            let task = scope.task()?;
            let mut assignment = scope_assignment(assignments, &scope)
                .cloned()
                .ok_or("choose a model for this first")?;
            assignment.options.reasoning_effort = effort;
            let assignment = checked(config, task, assignment)?;
            set_scope(assignments, &scope, Some(assignment));
            Ok(SideEffect::None)
        }
    }
}

/// Apply `operation` to the person's saved state and perform its side
/// effect. A key goes to the gateway (which seals it in the vault) and is
/// wiped here; a sign-in runs the browser flow on its own thread and reports
/// back to the pane.
///
/// # Errors
///
/// Returns the reason when the operation is refused or cannot be saved.
pub fn perform(operation: AiOperation) -> Result<(), String> {
    let mut config = ProviderConfig::load();
    let mut assignments = ModelAssignments::load();
    let effect = apply(operation, &mut config, &mut assignments)?;
    config
        .save()
        .map_err(|error| format!("provider settings were not saved: {error}"))?;
    assignments
        .save()
        .map_err(|error| format!("model assignments were not saved: {error}"))?;
    match &effect {
        SideEffect::None => Ok(()),
        SideEffect::StoreKey { profile, key } => {
            tilo_model_gateway::set_api_key(profile, key.clone())
                .map_err(|error| format!("the key was not saved: {error}"))
        }
        SideEffect::RemoveKey { profile } => tilo_model_gateway::remove_api_key(profile)
            .map_err(|error| format!("the key was not removed: {error}")),
        SideEffect::SignOut { account_id } => {
            forget_account(account_id);
            Ok(())
        }
        SideEffect::SignIn { profile } => start_sign_in(profile.clone()),
    }
}

fn forget_account(account_id: &str) {
    let id = accounts::AccountId::new(account_id);
    let mut vault = accounts::SecretVault::open();
    vault.remove(&id);
    vault.save();
    let mut store = accounts::AccountStore::load();
    if store.remove(&id) {
        let _ = store.save();
    }
}

/// Open the Microsoft sign-in in the browser and, once it completes, record
/// the account, seal its tokens and point the provider at it.
fn start_sign_in(profile: String) -> Result<(), String> {
    let client = accounts::oauth::ClientSlot::Entra
        .registration()
        .ok_or("Microsoft sign-in is not configured in this installation")?
        .client(agent_provider_config::AZURE_OPENAI_SCOPE);
    crate::panes::ai::set_signing_in(Some(profile.clone()));
    std::thread::spawn(move || {
        let result = accounts::oauth::run_loopback_authorization(
            &client,
            "",
            |url| ui::widget::try_open_external_url(url).map_err(|error| error.to_string()),
            std::time::Duration::from_secs(300),
        )
        .and_then(|tokens| {
            let account_id = format!("entra-{}", uuid::Uuid::new_v4());
            let id = accounts::AccountId::new(account_id.clone());
            let mut vault = accounts::SecretVault::open();
            vault.put(&id, &accounts::oauth::tokens_to_secret(&tokens));
            vault
                .try_save()
                .map_err(|error| format!("could not save the sign-in: {error}"))?;
            let mut account = accounts::Account::new(id, agent_provider_config::ENTRA_ACCOUNT_TYPE);
            account.display_name = "Microsoft Entra ID".into();
            let mut store = accounts::AccountStore::load();
            store.upsert(account);
            store
                .save()
                .map_err(|error| format!("could not save the account: {error}"))?;
            let mut config = ProviderConfig::load();
            let entry = profile_mut(&mut config, &profile)?;
            entry.credential = Some(Credential::Account { account_id });
            config
                .save()
                .map_err(|error| format!("provider settings were not saved: {error}"))
        });
        crate::panes::ai::set_signing_in(None);
        crate::panes::ai::notify(match result {
            Ok(()) => crate::i18n::t("ai-notice-signed-in"),
            Err(error) => {
                crate::i18n::t_args("ai-notice-failed", &[("error", crate::i18n::arg(error))])
            }
        });
    });
    Ok(())
}

/// Everything the AI hub shows and its resource tree exports.
#[derive(Clone, Debug)]
pub struct AiState {
    /// The person's profiles with this device's runtime merged in.
    pub config: ProviderConfig,
    pub assignments: ModelAssignments,
    /// The model manager's snapshot, or why it is unavailable.
    pub snapshot: Result<LocalModelSnapshot, String>,
    /// Per-provider health from the gateway, when probed.
    pub health: std::collections::BTreeMap<String, tilo_model_gateway::ProviderHealth>,
    /// Agents admitted to the person's workspaces.
    pub agents: Vec<String>,
}

impl AiState {
    /// Read the durable state and project `snapshot` into this device's
    /// runtime record, which the gateway resolves against.
    #[must_use]
    pub fn load(snapshot: Result<LocalModelSnapshot, String>) -> Self {
        if let Ok(snapshot) = &snapshot {
            let runtime = snapshot.runtime();
            if agent_provider_config::LocalRuntime::load() != runtime {
                let _ = runtime.save();
            }
        }
        Self {
            config: ProviderConfig::load(),
            assignments: ModelAssignments::load(),
            snapshot,
            health: std::collections::BTreeMap::new(),
            agents: crate::panes::permissions::agent_members(),
        }
    }

    /// The resolver's inputs, with this device's hardware facts.
    #[must_use]
    pub fn inputs(&self) -> assign::Inputs<'_> {
        assign::Inputs {
            config: &self.config,
            assignments: &self.assignments,
            hardware: self.snapshot.as_ref().ok().map(|snapshot| &snapshot.facts),
        }
    }

    /// What `task` resolves to for `agent` on this device.
    ///
    /// # Errors
    ///
    /// Returns the resolver's reason when nothing can serve the task.
    pub fn resolve(
        &self,
        task: &'static TaskSpec,
        agent: Option<&str>,
    ) -> Result<assign::Resolution, assign::ResolveError> {
        assign::resolve_task(&self.inputs(), task, agent)
    }

    /// Every configured target that could serve `task`, as `(value, profile,
    /// model)`; off-device targets only when the task's class admits them
    /// (`off_device`), local ones only when installed here.
    #[must_use]
    pub fn targets(
        &self,
        task: &'static TaskSpec,
        off_device: bool,
    ) -> Vec<(String, &ProviderProfile, &ModelDescriptor)> {
        let mut targets = Vec::new();
        for profile in &self.config.profiles {
            let local = profile.is_device_local();
            if !local && !off_device {
                continue;
            }
            for model in &profile.models {
                let target = if local {
                    ModelTarget::Local {
                        model_id: model.id.clone(),
                    }
                } else {
                    ModelTarget::Profile {
                        profile_id: profile.id.clone(),
                        model_id: model.id.clone(),
                    }
                };
                if task.requires.check(model).is_ok() {
                    targets.push((target_value(&target), profile, model));
                }
            }
        }
        targets
    }

    /// The best signed release this device can install for `task`, when no
    /// installed model serves it yet.
    #[must_use]
    pub fn installable_for(
        &self,
        task: &'static TaskSpec,
    ) -> Option<&agent_provider_config::manager::AvailableModel> {
        let snapshot = self.snapshot.as_ref().ok()?;
        let candidate = snapshot.best_for(task.requires.capabilities)?;
        let installed = snapshot
            .models
            .iter()
            .any(|model| model.package.id == candidate.package.id);
        (!installed).then_some(candidate)
    }
}

#[cfg(test)]
mod tests {
    use agent_provider_config::ModelOptions;
    use agent_provider_config::assign::Layer;
    use agent_provider_config::tasks::{AGENT_PLANNING, FEEDBACK_REVIEW, PHOTOS_VISION};

    use super::*;

    fn azure(config: &mut ProviderConfig, assignments: &mut ModelAssignments) -> String {
        apply(
            AiOperation::AddProvider {
                protocol: ProviderProtocol::AzureOpenAi,
                location: EndpointLocation::Hosted,
            },
            config,
            assignments,
        )
        .unwrap();
        let id = config.profiles.last().unwrap().id.clone();
        apply(
            AiOperation::SetProviderDetails {
                id: id.clone(),
                label: "Work Azure".into(),
                endpoint: "https://work.openai.azure.com/openai/v1".into(),
            },
            config,
            assignments,
        )
        .unwrap();
        apply(
            AiOperation::AddModel {
                profile: id.clone(),
                model: "gpt-5.6-sol".into(),
                context_window: Some(400_000),
            },
            config,
            assignments,
        )
        .unwrap();
        id
    }

    #[test]
    fn an_agent_gets_its_own_azure_deployment_in_fast_mode() {
        let mut config = ProviderConfig::default();
        let mut assignments = ModelAssignments::default();
        let id = azure(&mut config, &mut assignments);
        let scope = AssignScope::Agent {
            agent: "X".into(),
            task: AGENT_PLANNING.id.into(),
        };
        let target = parse_target(&format!("{id}/gpt-5.6-sol")).unwrap();
        apply(
            AiOperation::ChooseModel {
                scope: scope.clone(),
                target: Some(target),
            },
            &mut config,
            &mut assignments,
        )
        .unwrap();
        // Fast mode is refused until the deployment declares it.
        assert!(
            apply(
                AiOperation::SetFastMode {
                    scope: scope.clone(),
                    enabled: true
                },
                &mut config,
                &mut assignments
            )
            .is_err()
        );
        apply(
            AiOperation::SetModelFeature {
                profile: id,
                model: "gpt-5.6-sol".into(),
                feature: ModelFeature::FastMode,
                enabled: true,
            },
            &mut config,
            &mut assignments,
        )
        .unwrap();
        apply(
            AiOperation::SetFastMode {
                scope,
                enabled: true,
            },
            &mut config,
            &mut assignments,
        )
        .unwrap();
        let inputs = assign::Inputs {
            config: &config,
            assignments: &assignments,
            hardware: None,
        };
        let resolved = assign::resolve(
            &inputs,
            &tasks::Consumer::app(tasks::AGENT_APP),
            AGENT_PLANNING.id,
            Some("X"),
        )
        .unwrap();
        assert_eq!(resolved.layer, Layer::Agent);
        assert_eq!(
            resolved.options,
            ModelOptions {
                fast_mode: true,
                reasoning_effort: None
            }
        );
    }

    #[test]
    fn a_sensitive_task_needs_the_explicit_allowance_and_a_device_only_task_refuses_it() {
        let mut config = ProviderConfig::default();
        let mut assignments = ModelAssignments::default();
        let id = azure(&mut config, &mut assignments);
        apply(
            AiOperation::SetModelCapability {
                profile: id.clone(),
                model: "gpt-5.6-sol".into(),
                capability: ModelCapability::TraceReview,
                declared: true,
            },
            &mut config,
            &mut assignments,
        )
        .unwrap();
        let target = parse_target(&format!("{id}/gpt-5.6-sol")).unwrap();
        let review = AssignScope::Task {
            task: FEEDBACK_REVIEW.id.into(),
        };
        let chosen = apply(
            AiOperation::ChooseModel {
                scope: review.clone(),
                target: Some(target.clone()),
            },
            &mut config,
            &mut assignments,
        );
        assert!(
            chosen
                .unwrap_err()
                .contains("allow this task to run off this device")
        );
        apply(
            AiOperation::AllowOffDevice {
                scope: review,
                target: target.clone(),
            },
            &mut config,
            &mut assignments,
        )
        .unwrap();
        assert!(assignments.tasks[FEEDBACK_REVIEW.id].allow_off_device);
        assert!(
            apply(
                AiOperation::AllowOffDevice {
                    scope: AssignScope::Task {
                        task: PHOTOS_VISION.id.into()
                    },
                    target,
                },
                &mut config,
                &mut assignments
            )
            .unwrap_err()
            .contains("never leaves")
        );
    }

    #[test]
    fn removing_a_provider_forgets_its_assignments_and_its_key() {
        let mut config = ProviderConfig::default();
        let mut assignments = ModelAssignments::default();
        let id = azure(&mut config, &mut assignments);
        apply(
            AiOperation::ChooseModel {
                scope: AssignScope::Task {
                    task: AGENT_PLANNING.id.into(),
                },
                target: parse_target(&format!("{id}/gpt-5.6-sol")),
            },
            &mut config,
            &mut assignments,
        )
        .unwrap();
        let effect = apply(
            AiOperation::RemoveProvider { id: id.clone() },
            &mut config,
            &mut assignments,
        )
        .unwrap();
        assert_eq!(effect, SideEffect::RemoveKey { profile: id });
        assert!(assignments.tasks.is_empty());
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn a_key_never_reaches_the_settings_document_or_a_log() {
        let mut config = ProviderConfig::default();
        let mut assignments = ModelAssignments::default();
        let id = azure(&mut config, &mut assignments);
        let operation = AiOperation::SetApiKey {
            profile: id.clone(),
            key: "sk-fixture-value".into(),
        };
        assert!(!format!("{operation:?}").contains("sk-fixture-value"));
        let effect = apply(operation, &mut config, &mut assignments).unwrap();
        assert!(matches!(effect, SideEffect::StoreKey { .. }));
        assert!(
            !serde_json::to_string(&config)
                .unwrap()
                .contains("sk-fixture-value")
        );
        assert!(
            apply(
                AiOperation::SignIn { profile: id },
                &mut config,
                &mut assignments
            )
            .is_ok()
        );
    }

    #[test]
    fn targets_round_trip_through_their_action_spelling() {
        for target in [
            ModelTarget::Local {
                model_id: "qwen3-4b".into(),
            },
            ModelTarget::Profile {
                profile_id: "p".into(),
                model_id: "m/with/slashes".into(),
            },
        ] {
            assert_eq!(parse_target(&target_value(&target)), Some(target));
        }
        assert_eq!(parse_target("local:"), None);
        assert_eq!(parse_target("nothing"), None);
    }
}
