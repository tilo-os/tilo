//! Owner-private task text. Never serialize this store into room publications.
use crate::{NativeWorkspaceOwner, RoomId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::SystemTime};
use tilo_desktop_model::{WorkspaceTaskMutationResult, WorkspaceTaskRoute};
use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;

const MAX_PRIVATE_TASKS: usize = 4096;

/// Owner-private task content and approved model snapshots. Never published.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PrivateTaskTextStore {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) model_authorities:
        BTreeMap<String, tilo_desktop_model::task::WorkspaceTaskModelAuthority>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) confirmations: crate::private_task_confirmation::Confirmations,
    #[serde(default)]
    instructions: BTreeMap<String, NativePrivateTaskInstructions>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) reports: BTreeMap<String, crate::private_task_outputs::NativePrivateTaskReport>,
    /// Exact recovered audit events, without invented authenticated report identities.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) historical_reports: BTreeMap<String, Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) historical_confirmations:
        BTreeMap<String, Vec<tilo_desktop_model::WorkspaceTaskConfirmation>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) historical_confirmation_audit: BTreeMap<String, Vec<serde_json::Value>>,
}

fn private_report_event(event: &serde_json::Value) -> bool {
    if let Some(presentation) = event.get("presentation") {
        return ["text", "reasoning", "detail"].iter().any(|field| {
            presentation
                .get(field)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| !text.is_empty())
        });
    }
    let Some(note) = event.get("note").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Ok(parsed) =
        serde_json::from_value::<tilo_desktop_model::WorkspaceTaskAuditEvent>(event.clone())
    else {
        return true;
    };
    let neutral = match parsed.action() {
        "finish" => match event
            .pointer("/what/args/outcome")
            .and_then(serde_json::Value::as_str)
        {
            Some("done") => "done",
            Some("cancelled") => "cancelled",
            _ => "waiting",
        },
        "abandon" => "abandoned",
        _ => "progress",
    };
    note != neutral
}

fn private_confirmation_event(event: &serde_json::Value) -> bool {
    fn what(value: &serde_json::Value) -> (bool, bool) {
        let confirmation = matches!(
            value
                .pointer("/action/action")
                .and_then(serde_json::Value::as_str),
            Some("ask-confirmation" | "confirm-action" | "decline-action")
        );
        let mut private = confirmation
            && value
                .get("args")
                .is_some_and(|args| args.as_object().is_none_or(|args| !args.is_empty()));
        let mut affected = confirmation;
        if let Some(members) = value.get("members").and_then(serde_json::Value::as_array) {
            for member in members {
                let (has, secret) = what(member);
                affected |= has;
                private |= secret;
            }
        }
        if let Some(previous) = value.pointer("/of/what") {
            let (has, secret) = what(previous);
            affected |= has;
            private |= secret;
        }
        (affected, private)
    }
    let (affected, private) = event.get("what").map_or((false, false), what);
    private || (affected && event.get("note").is_some_and(|note| !note.is_null()))
}

impl std::fmt::Debug for PrivateTaskTextStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateTaskTextStore { private: redacted }")
    }
}

/// Private task text is retained in owner storage, never a room document.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NativePrivateTaskInstructions {
    pub instruction: Option<String>,
    #[serde(default)]
    pub agent_request: Option<tilo_desktop_model::WorkspaceAgentRequest>,
    pub directions: BTreeMap<u64, String>,
}
impl std::fmt::Debug for NativePrivateTaskInstructions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativePrivateTaskInstructions { private: redacted }")
    }
}
/// Prepared text write; only a successful canonical reducer supplies its task id.
pub struct NativePrivateInstructionWrite {
    instruction: Option<String>,
    agent_request: Option<tilo_desktop_model::WorkspaceAgentRequest>,
    direction: Option<String>,
}
type PreparedPrivateCommand = (
    tilo_peer_state::workspace_task::WorkspaceTaskCommand,
    Option<NativePrivateInstructionWrite>,
);

impl NativePrivateInstructionWrite {
    pub fn instruction_reference(&self) -> Option<String> {
        self.instruction
            .as_deref()
            .map(tilo_peer_state::workspace_task::private_instruction_reference)
    }
}
impl PrivateTaskTextStore {
    /// Adapt the governor's private text file without publishing its bodies.
    pub fn from_instructions(
        instructions: BTreeMap<String, NativePrivateTaskInstructions>,
    ) -> Self {
        Self {
            instructions,
            model_authorities: BTreeMap::new(),
            confirmations: BTreeMap::new(),
            reports: BTreeMap::new(),
            historical_reports: BTreeMap::new(),
            historical_confirmations: BTreeMap::new(),
            historical_confirmation_audit: BTreeMap::new(),
        }
    }
    /// Owner-private persistence only; never send this map to a room subscriber.
    pub fn instructions(&self) -> &BTreeMap<String, NativePrivateTaskInstructions> {
        &self.instructions
    }
    /// Refuse oversized owner-private collections on write and restore.
    pub(crate) fn validate(&self) -> Result<(), String> {
        crate::private_model_authority::validate(&self.model_authorities)?;
        crate::private_task_confirmation::validate(&self.confirmations)?;
        crate::private_task_confirmation::validate_archives(
            &self.historical_confirmations,
            &self.historical_confirmation_audit,
        )?;
        crate::private_task_outputs::validate_reports(&self.reports)?;
        crate::private_task_outputs::validate_historical_reports(&self.historical_reports)?;
        // Archives retain their original envelopes, including extension fields. Validate
        // their prose without normalizing them or inventing authenticated report metadata.
        for events in self.historical_reports.values() {
            for event in events {
                let note = event["note"]
                    .as_str()
                    .ok_or("invalid historical report note")?;
                if note.len()
                    > tilo_actions::task_messaging::TaskReportPresentation::MAX_PROSE_BYTES
                {
                    return Err("oversized private historical task report".into());
                }
                if let Some(presentation) = event.get("presentation") {
                    let presentation: tilo_actions::task_messaging::TaskReportPresentation =
                        serde_json::from_value(presentation.clone())
                            .map_err(|_| "invalid historical task presentation")?;
                    presentation.validate()?;
                }
            }
        }
        if self.instructions.len() > MAX_PRIVATE_TASKS {
            return Err("private task store capacity exhausted".into());
        }
        for private in self.instructions.values() {
            if let Some(instruction) = &private.instruction {
                validate_text(
                    instruction,
                    tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
                )?;
            }
            if let Some(request) = &private.agent_request {
                validate_text(
                    &request.instruction,
                    tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
                )?;
                if private
                    .instruction
                    .as_ref()
                    .is_some_and(|text| text != &request.instruction)
                {
                    return Err("private task instruction and request disagree".into());
                }
            }
            for (revision, text) in &private.directions {
                if *revision == 0 {
                    return Err("private task direction has no canonical revision".into());
                }
                validate_text(text, tilo_desktop_model::MAX_WORKSPACE_TASK_DIRECTION_BYTES)?;
            }
        }
        Ok(())
    }

    /// Public references backed by retained private text, never guessed from labels.
    pub(crate) fn private_references(
        &self,
    ) -> tilo_peer_state::workspace_task::NativeTaskPrivateReferences {
        self.instructions
            .iter()
            .map(|(task, value)| {
                (
                    task.clone(),
                    value
                        .directions
                        .iter()
                        .map(|(revision, text)| {
                            (
                                *revision,
                                tilo_peer_state::workspace_task::private_instruction_reference(
                                    text,
                                ),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    }
    /// Whether extraction recovered private material rather than only already
    /// scrubbed confirmation placeholders. Prevents needless baseline generations.
    pub fn migration_has_private_text(
        text: &BTreeMap<String, tilo_peer_state::workspace_task::NativeTaskPrivateText>,
    ) -> bool {
        text.values().any(|value| {
            value.model_authority.is_some()
                || value.instruction.is_some()
                || value.agent_request.is_some()
                || !value.directions.is_empty()
                || value.historical_reports.iter().any(private_report_event)
                || value.confirmation.is_some()
                || value.historical_confirmations.iter().any(|full| {
                    crate::private_task_confirmation::public_confirmation(full) != *full
                })
                || value
                    .historical_confirmation_audit
                    .iter()
                    .any(private_confirmation_event)
        })
    }

    /// Retain text extracted from historical current room state.
    pub(crate) fn retain_migrated_text(
        &mut self,
        text: BTreeMap<String, tilo_peer_state::workspace_task::NativeTaskPrivateText>,
    ) -> Result<(), String> {
        for (task, text) in text {
            if let Some(full) = text.model_authority {
                self.model_authorities.insert(task.clone(), full);
            }
            if let Some(full) = text.confirmation {
                // Only a recovered *current* value may hydrate current policy.
                // Archived answers never acquire current invocation authority.
                if let Some(existing) = self.confirmations.get(&task)
                    && crate::private_task_confirmation::public_confirmation(existing)
                        == crate::private_task_confirmation::public_confirmation(&full)
                    && existing != &full
                {
                    return Err(
                        "recovered private confirmation conflicts with retained custody".into(),
                    );
                }
                self.confirmations.insert(task.clone(), full);
            }
            let archive = self
                .historical_confirmations
                .entry(task.clone())
                .or_default();
            for full in text.historical_confirmations {
                if !archive.contains(&full) {
                    archive.push(full);
                }
            }
            let audit = self
                .historical_confirmation_audit
                .entry(task.clone())
                .or_default();
            for event in text.historical_confirmation_audit {
                if !audit.contains(&event) {
                    audit.push(event);
                }
            }
            if !text.historical_reports.is_empty() {
                let archive = self.historical_reports.entry(task.clone()).or_default();
                for event in text.historical_reports {
                    if !archive.contains(&event) {
                        archive.push(event);
                    }
                }
            }
            let value = self.instructions.entry(task).or_default();
            if let Some(instruction) = text.instruction {
                value.instruction = Some(instruction);
            }
            if let Some(request) = text.agent_request {
                value.agent_request = Some(request);
            }
            value.directions.extend(text.directions);
        }
        self.validate()
    }
    /// Strip model instructions before the reducer can create CRDT tombstones.
    /// Human coordination on manual tasks stays unchanged.
    /// # Errors
    /// Returns an error if the private instruction or admitted task frontier is invalid.
    pub fn prepare_private_command(
        &self,
        command: &tilo_peer_state::workspace_task::WorkspaceTaskCommand,
        peer: &crate::StatePeer,
    ) -> Result<PreparedPrivateCommand, String> {
        use tilo_peer_state::workspace_task::WorkspaceTaskCommand as C;
        self.validate()?;
        match command {
            C::Queue {
                title,
                assigned_agent,
                route,
                resource,
            } if resource
                .as_ref()
                .is_some_and(|resource| resource.agent_request.is_some()) =>
            {
                Self::prepare_private_agent_request(title, assigned_agent, *route, resource.clone())
            }
            C::Queue { title, .. } => Ok((
                command.clone(),
                Some(NativePrivateInstructionWrite {
                    // A title-only request is still a real instruction. Retain
                    // it in the same owner custody used to bind model dispatch.
                    instruction: Some(title.clone()),
                    agent_request: None,
                    direction: None,
                }),
            )),
            C::QueuePrivateSubtask {
                parent_task_id,
                title,
                instruction,
                assigned_agent,
                route,
            } => Self::prepare_private_subtask(
                parent_task_id,
                title,
                instruction,
                assigned_agent,
                *route,
            ),
            C::QueueInstruction {
                title,
                instruction,
                assigned_agent,
                route,
            } => {
                validate_text(
                    instruction,
                    tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
                )?;
                Ok((
                    C::Queue {
                        title: public_title(title, instruction),
                        assigned_agent: assigned_agent.clone(),
                        route: *route,
                        resource: None,
                    },
                    Some(NativePrivateInstructionWrite {
                        instruction: Some(instruction.clone()),
                        agent_request: None,
                        direction: None,
                    }),
                ))
            }
            C::QueueInstructionAbout {
                title,
                instruction,
                assigned_agent,
                route,
                subject,
            } => {
                validate_text(
                    instruction,
                    tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
                )?;
                Ok((
                    C::QueueInstructionAbout {
                        title: public_title(title, instruction),
                        instruction: tilo_peer_state::workspace_task::private_instruction_reference(
                            instruction,
                        ),
                        assigned_agent: assigned_agent.clone(),
                        route: *route,
                        subject: subject.clone(),
                    },
                    Some(NativePrivateInstructionWrite {
                        instruction: Some(instruction.clone()),
                        agent_request: None,
                        direction: None,
                    }),
                ))
            }
            C::Direction { task_id, text } | C::DirectionForRun { task_id, text, .. } => {
                Self::prepare_private_direction(command, task_id, text, peer)
            }
            _ => Ok((command.clone(), None)),
        }
    }
    fn prepare_private_subtask(
        parent_task_id: &str,
        title: &str,
        instruction: &str,
        assigned_agent: &str,
        route: tilo_desktop_model::WorkspaceTaskRoute,
    ) -> Result<PreparedPrivateCommand, String> {
        use tilo_peer_state::workspace_task::WorkspaceTaskCommand as C;
        validate_text(
            instruction,
            tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
        )?;
        Ok((
            C::CreateSubtask {
                parent_task_id: parent_task_id.to_owned(),
                title: public_title(title, instruction),
                assigned_agent: Some(assigned_agent.to_owned()),
                route: Some(route),
            },
            Some(NativePrivateInstructionWrite {
                instruction: Some(instruction.to_owned()),
                agent_request: None,
                direction: None,
            }),
        ))
    }

    /// Resolve a canonical parent only from retained owner-private storage.
    /// Matching prose or public resource metadata never identifies a retry.
    pub(crate) fn retained_chat_retry(
        &self,
        command: &tilo_peer_state::workspace_task::WorkspaceTaskCommand,
        peer: &crate::StatePeer,
        authority: &WorkspaceTaskAuthority,
        now: SystemTime,
    ) -> Result<Option<&str>, String> {
        let tilo_peer_state::workspace_task::WorkspaceTaskCommand::Queue { resource, .. } = command
        else {
            return Ok(None);
        };
        let Some(request) = resource
            .as_ref()
            .and_then(|resource| resource.agent_request.as_ref())
        else {
            return Ok(None);
        };
        let WorkspaceTaskAuthority::Person { caller } = authority else {
            return Err("private Chat task requires its original person".into());
        };
        if !peer.certificates().contains(caller) {
            return Err("private Chat caller certificate changed".into());
        }
        let member = peer.permissions(caller.device_public_key, now)?;
        if !member.allows(
            &tilo_action_policy::TreePath::room(peer.room()),
            tilo_action_policy::ActionKind::Undoable,
            now,
        ) {
            return Err("private Chat task person lacks write authority".into());
        }
        let snapshot = tilo_peer_state::workspace_task::snapshot(
            peer.admitted_room()
                .ok_or("private Chat task room absent")?,
        )?;
        let mut found = None;
        for (task, private) in &self.instructions {
            let Some(prior) = &private.agent_request else {
                continue;
            };
            if prior.chat_record != request.chat_record || prior.message_id != request.message_id {
                continue;
            }
            // Check ownership before comparing any retained private prose.
            if !snapshot
                .tasks
                .iter()
                .any(|value| value.id == *task && value.created_by == member.person)
                || prior != request
            {
                return Err(
                    "canonical Chat parent is already bound to another task request".into(),
                );
            }
            if found.replace(task.as_str()).is_some() {
                return Err("canonical Chat parent has ambiguous private custody".into());
            }
        }
        Ok(found)
    }

    fn prepare_private_agent_request(
        title: &str,
        assigned_agent: &str,
        route: WorkspaceTaskRoute,
        mut resource: Option<Box<tilo_desktop_model::WorkspaceTaskResource>>,
    ) -> Result<PreparedPrivateCommand, String> {
        use tilo_peer_state::workspace_task::WorkspaceTaskCommand as C;
        let request = resource
            .as_mut()
            .and_then(|resource| resource.agent_request.take())
            .ok_or("private agent request absent")?;
        validate_text(
            &request.instruction,
            tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
        )?;
        if request.invocation.0.is_nil()
            || request.chat_record.is_empty()
            || request.channel_id.is_empty()
            || request.message_id != format!("m_{}", request.invocation.0)
        {
            return Err("task source lacks its canonical Chat parent".into());
        }
        Ok((
            C::Queue {
                title: public_title(title, &request.instruction),
                assigned_agent: assigned_agent.to_owned(),
                route,
                resource,
            },
            Some(NativePrivateInstructionWrite {
                instruction: Some(request.instruction.clone()),
                agent_request: Some(request),
                direction: None,
            }),
        ))
    }

    fn prepare_private_direction(
        command: &tilo_peer_state::workspace_task::WorkspaceTaskCommand,
        task_id: &str,
        text: &str,
        peer: &crate::StatePeer,
    ) -> Result<PreparedPrivateCommand, String> {
        use tilo_peer_state::workspace_task::WorkspaceTaskCommand as C;
        let state = tilo_peer_state::workspace_task::snapshot(
            peer.admitted_room().ok_or("private task document absent")?,
        )?;
        if state.tasks.iter().any(|task| {
            task.id == task_id && task.route != tilo_desktop_model::WorkspaceTaskRoute::Manual
        }) {
            validate_text(text, tilo_desktop_model::MAX_WORKSPACE_TASK_DIRECTION_BYTES)?;
            let reference = tilo_peer_state::workspace_task::private_instruction_reference(text);
            let mut public_command = command.clone();
            match &mut public_command {
                C::Direction { text, .. } | C::DirectionForRun { text, .. } => {
                    *text = reference;
                }
                _ => return Err("private direction command changed variant".into()),
            }
            Ok((
                public_command,
                Some(NativePrivateInstructionWrite {
                    instruction: None,
                    agent_request: None,
                    direction: Some(text.to_owned()),
                }),
            ))
        } else {
            Ok((command.clone(), None))
        }
    }

    /// Retain private text only after the real reducer accepted its command.
    /// # Errors
    /// Returns an error if the write has no canonical task or exceeds storage capacity.
    pub fn retain_private_instruction(
        &mut self,
        result: &tilo_desktop_model::WorkspaceTaskMutationResult,
        write: NativePrivateInstructionWrite,
    ) -> Result<(), String> {
        let task = match result {
            WorkspaceTaskMutationResult::Queued { task_id } if write.direction.is_none() => task_id,
            WorkspaceTaskMutationResult::DirectionAdded { task_id, .. }
                if write.direction.is_some() =>
            {
                task_id
            }
            _ => return Err("private task write has no canonical accepted result".into()),
        };
        let value = self.instructions.entry(task.clone()).or_default();
        if let Some(instruction) = write.instruction {
            value.instruction = Some(instruction);
        }
        if let Some(request) = write.agent_request {
            value.agent_request = Some(request);
        }
        if let Some(text) = write.direction {
            let WorkspaceTaskMutationResult::DirectionAdded { revision, .. } = result else {
                return Err("private direction has no canonical revision".into());
            };
            value.directions.insert(*revision, text);
        }
        self.validate()
    }
}

// Earlier callers derived a public title from the first line of private text.
// Do not reproduce that leak for one-line instructions or truncated excerpts.
fn public_title(title: &str, instruction: &str) -> String {
    if tilo_peer_state::workspace_task::legacy_private_task_title(title, instruction) {
        "Agent task".into()
    } else {
        title.to_owned()
    }
}

fn validate_text(text: &str, max: usize) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > max {
        return Err(format!("private task text is empty or exceeds {max} bytes"));
    }
    Ok(())
}

/// Defense in depth for older records before their signed migration. This is a
/// public DTO only; private reads never hydrate this snapshot.
pub(crate) fn redact_snapshot(
    snapshot: &mut tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
    view: &crate::NativeCommittedView,
) {
    let references = view.record.private_task_text.private_references();
    let legacy_instructions: BTreeMap<_, _> = snapshot
        .controls
        .iter()
        .filter_map(|(id, control)| control.instruction.clone().map(|text| (id.clone(), text)))
        .collect();
    for control in snapshot.controls.values_mut() {
        if let Some(confirmation) = &mut control.confirmation {
            *confirmation = crate::private_task_confirmation::public_confirmation(confirmation);
        }
        control.instruction = None;
        if let Some(model) = &mut control.model_authority {
            // Serialization of a JSON value cannot fail, but fail closed rather
            // than exposing a profile if this ever changes.
            model.contract =
                tilo_peer_state::workspace_task::private_model_contract_reference(&model.contract)
                    .unwrap_or(serde_json::Value::Null);
        }
    }
    for task in &mut snapshot.tasks {
        let request = task
            .resource
            .as_mut()
            .and_then(|resource| resource.agent_request.take());
        if task.route != WorkspaceTaskRoute::Manual
            && let Some(control) = snapshot.controls.get_mut(&task.id)
        {
            for direction in &mut control.directions {
                let signed_reference = view.record.sanitized_baseline.is_some()
                    && direction
                        .text
                        .strip_prefix("private-instruction:")
                        .is_some_and(|hash| {
                            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                        });
                if !signed_reference
                    && references
                        .get(&task.id)
                        .and_then(|values| values.get(&direction.revision))
                        != Some(&direction.text)
                {
                    direction.text = tilo_peer_state::workspace_task::private_instruction_reference(
                        &direction.text,
                    );
                }
            }
        }
        let private = view.record.private_task_text.instructions.get(&task.id);
        let instruction = private
            .and_then(|text| {
                text.instruction.as_deref().or_else(|| {
                    text.agent_request
                        .as_ref()
                        .map(|request| request.instruction.as_str())
                })
            })
            .or_else(|| legacy_instructions.get(&task.id).map(String::as_str))
            .or_else(|| request.as_ref().map(|request| request.instruction.as_str()));
        if task.route == WorkspaceTaskRoute::Model
            && view.record.sanitized_baseline.is_none()
            && let Some(instruction) = instruction
        {
            if tilo_peer_state::workspace_task::legacy_private_task_title(&task.title, instruction)
            {
                task.title = "Model task".into();
            }
            for event in &mut task.audit {
                redact_audit_title(&mut event.what, instruction);
            }
        }
    }
}

fn redact_audit_title(what: &mut tilo_actions::Did, instruction: &str) {
    match what {
        tilo_actions::Did::Verb(verb) => {
            if let Some(title) = verb.args.get_mut("title")
                && title.as_str().is_some_and(|title| {
                    tilo_peer_state::workspace_task::legacy_private_task_title(title, instruction)
                })
            {
                *title = tilo_actions::Scalar::text("Model task");
            }
        }
        tilo_actions::Did::Group(group) => {
            for member in &mut group.members {
                redact_audit_title(member, instruction);
            }
        }
        tilo_actions::Did::History(step) => {
            if let Some(of) = &mut step.of {
                redact_audit_title(&mut of.what, instruction);
            }
        }
        tilo_actions::Did::Edit(_) => {}
    }
}

fn verify_private_instruction(
    private: Option<&NativePrivateTaskInstructions>,
    reference: Option<&String>,
) -> Result<(), String> {
    if let Some(reference) = reference {
        let instruction = private
            .and_then(|text| {
                text.instruction.as_deref().or_else(|| {
                    text.agent_request
                        .as_ref()
                        .map(|request| request.instruction.as_str())
                })
            })
            .ok_or("private task instruction unavailable")?;
        if tilo_peer_state::workspace_task::private_instruction_reference(instruction) != *reference
            || private
                .and_then(|text| text.agent_request.as_ref())
                .is_some_and(|request| {
                    tilo_peer_state::workspace_task::private_instruction_reference(
                        &request.instruction,
                    ) != *reference
                })
        {
            return Err("private task instruction does not match public binding".into());
        }
    }
    Ok(())
}

impl NativeWorkspaceOwner {
    /// Read one exact task/run's private instructions under current custody.
    ///
    /// Authority must come from authenticated native transport, not app arguments.
    /// Host-only authority cannot read instructions. Agents require exact active
    /// run ownership; people require current write authority and creator or
    /// current run approval identity. Nothing is added to the public snapshot.
    ///
    /// # Errors
    /// Refuses fenced rooms, future epochs, revoked certificates, changed host
    /// pins, unassigned agents and unrelated people.
    pub fn workspace_task_private_instructions(
        &self,
        room: RoomId,
        task_id: &str,
        run_epoch: u64,
        authority: &WorkspaceTaskAuthority,
        now: SystemTime,
    ) -> Result<Option<NativePrivateTaskInstructions>, String> {
        self.with_current(room, |view| {
            view.workspace_task_private_instructions(room, task_id, run_epoch, authority, now)
        })
    }
}

impl crate::NativeCommittedView {
    /// Authorize and read private text from this exact committed generation.
    /// Callers can read other private state under the same current-view guard
    /// without waiting for an in-flight owner's durable mutation.
    pub(crate) fn workspace_task_private_instructions(
        &self,
        room: RoomId,
        task_id: &str,
        run_epoch: u64,
        authority: &WorkspaceTaskAuthority,
        now: SystemTime,
    ) -> Result<Option<NativePrivateTaskInstructions>, String> {
        let cert = authority.certificate();
        if !self
            .peer
            .certificates()
            .iter()
            .any(|current| current == cert)
        {
            return Err("private task reader lacks exact current certificate".into());
        }
        // permissions rechecks certificate expiry and revocation as well as
        // current room membership, including the attesting device.
        let member = self.peer.permissions(cert.device_public_key, now)?;
        let state = tilo_peer_state::workspace_task::snapshot(
            self.peer
                .admitted_room()
                .ok_or("private task document absent")?,
        )?;
        let task = state
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .ok_or("private task absent")?;
        let control = state
            .controls
            .get(task_id)
            .ok_or("private task control absent")?;
        if run_epoch > control.run_epoch {
            return Err("private task run epoch has not existed".into());
        }
        match authority {
            WorkspaceTaskAuthority::Agent { attestor, agent_id } => {
                let pinned = match self.host() {
                    Some(host) => host == attestor && attestor.capabilities.can_act_as_host,
                    None => self.governor() == attestor,
                };
                let run = control
                    .active_run
                    .as_ref()
                    .ok_or("private task has no active run")?;
                if !pinned
                    || task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
                    || control.run_epoch != run_epoch
                    || run.epoch != run_epoch
                    || run.agent_id != *agent_id
                    || task.assigned_agent.as_deref() != Some(agent_id.as_str())
                {
                    return Err("private task reader is not its current run actor".into());
                }
                self.peer.workspace().check_task_execution_actor(
                    agent_id,
                    run.approval.as_ref(),
                    now,
                )?;
            }
            WorkspaceTaskAuthority::Person { .. } => {
                let starter = control
                    .active_run
                    .as_ref()
                    .and_then(|run| run.approval.as_ref())
                    .filter(|_| control.run_epoch == run_epoch)
                    .is_some_and(|approval| approval.person == member.person)
                    || task.audit.iter().any(|event| {
                        event.actor == member.person
                            && event.action() == "approve-run"
                            && matches!(&event.what, tilo_actions::Did::Verb(verb)
                                if verb.args.get("run") == tilo_actions::Scalar::of(&run_epoch).as_ref())
                    });
                if (task.created_by != member.person && !starter)
                    || !member.allows(
                        &tilo_action_policy::TreePath::room(room.to_string()),
                        tilo_action_policy::ActionKind::Undoable,
                        now,
                    )
                {
                    return Err("private task reader is not its authorized person".into());
                }
            }
            WorkspaceTaskAuthority::Host { .. } => {
                return Err("host-only authority cannot read private task text".into());
            }
        }
        let private = self.record.private_task_text.instructions.get(task_id);
        verify_private_instruction(private, control.private_instruction_ref.as_ref())?;
        Ok(private.cloned())
    }
}

#[cfg(test)]
#[path = "private_recovery_tests.rs"]
mod private_recovery_tests;
#[cfg(test)]
#[path = "sanitized_baseline_tests.rs"]
mod sanitized_baseline_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ROOM, raw_owner_fixture};
    use ed25519_dalek::SigningKey;
    use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
    use tilo_peer_state::workspace_task::WorkspaceTaskCommand;

    #[test]
    fn migrated_events_are_private_archives_not_authenticated_reports() {
        let marker = "exact historical report\n  with whitespace";
        let event = serde_json::json!({
            "note": marker,
            "actor": "host",
            "older_extension": {"retain": true},
        });
        let text = BTreeMap::from([(
            "retired-task".into(),
            tilo_peer_state::workspace_task::NativeTaskPrivateText {
                historical_reports: vec![event.clone()],
                ..Default::default()
            },
        )]);
        let mut store = PrivateTaskTextStore::default();
        store.retain_migrated_text(text.clone()).unwrap();
        store.retain_migrated_text(text).unwrap();
        let bytes = serde_json::to_vec(&store).unwrap();
        let restored: PrivateTaskTextStore = serde_json::from_slice(&bytes).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.historical_reports["retired-task"], vec![event]);
        assert!(restored.reports.is_empty());
        assert!(!format!("{restored:?}").contains(marker));
        let older: PrivateTaskTextStore = serde_json::from_str(r#"{"instructions":{}}"#).unwrap();
        older.validate().unwrap();
        assert!(older.historical_reports.is_empty());
    }

    #[test]
    fn historical_report_validation_is_exact_bounded_and_fail_closed() {
        let presentation = tilo_actions::task_messaging::TaskReportPresentation {
            run: "historic-turn".into(),
            sequence: 7,
            received_at: Some(123),
            text: "é".repeat(3072),
            reasoning: "é".repeat(512),
            finish: Some(tilo_actions::task_messaging::TaskReportPresentationFinish::Cancelled),
            detail: "é".repeat(512),
        };
        let event = serde_json::json!({
            "note": "é".repeat(4096),
            "presentation": presentation,
            "older_extension": {"retain": true},
        });
        let mut store = PrivateTaskTextStore::default();
        store
            .historical_reports
            .insert("retired-task".into(), vec![event.clone()]);
        store.validate().unwrap();
        let encoded = serde_json::to_vec(&store).unwrap();
        let restored: PrivateTaskTextStore = serde_json::from_slice(&encoded).unwrap();
        restored.validate().unwrap();
        assert_eq!(
            restored.historical_reports["retired-task"],
            vec![event.clone()]
        );
        assert!(restored.reports.is_empty());
        for pointer in [
            "/note",
            "/presentation/text",
            "/presentation/reasoning",
            "/presentation/detail",
        ] {
            let mut invalid = event.clone();
            let field = invalid.pointer_mut(pointer).unwrap();
            *field = format!("{}x", field.as_str().unwrap()).into();
            store
                .historical_reports
                .insert("retired-task".into(), vec![invalid]);
            let before = serde_json::to_vec(&store).unwrap();
            assert!(store.validate().is_err(), "{pointer}");
            assert_eq!(serde_json::to_vec(&store).unwrap(), before);
        }
        for malformed in [serde_json::json!({"text": 9}), serde_json::Value::Null] {
            let mut invalid = event.clone();
            invalid["presentation"] = malformed;
            store
                .historical_reports
                .insert("retired-task".into(), vec![invalid]);
            assert!(store.validate().is_err());
        }
    }

    pub(super) fn enrolled() -> (tempfile::TempDir, NativeWorkspaceOwner, RoomId, SystemTime) {
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let owner = crate::open_file(&directory.path().join("rooms.json")).unwrap();
        let (intent, bootstrap, _, _, now) = raw_owner_fixture();
        let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
        let enrollment = crate::testing::finalize(intent, pending, bootstrap).unwrap();
        owner.install(&enrollment, now).unwrap();
        let room = RoomId::parse(ROOM).unwrap();
        let mut candidate = owner.with_current(room, |view| Ok(view.clone())).unwrap();
        candidate
            .peer
            .governance_mut()
            .put_governed_grants(
                &[tilo_action_policy::Grant::new(
                    "private-text-test-agent",
                    tilo_action_policy::Subject::agent("scribe"),
                    tilo_action_policy::TreePath::room(ROOM),
                    tilo_action_policy::KindSet::READ_UNDOABLE,
                    tilo_action_policy::Answer::Allow,
                    tilo_action_policy::Subject::person("governor"),
                )],
                &SigningKey::from_bytes(&[61; 32]),
                now,
            )
            .unwrap();
        candidate.record.peer = candidate.peer.durable().unwrap();
        candidate.record.generation = candidate.record.generation.checked_add(1).unwrap();
        owner.publish_room(room, candidate, None).unwrap();
        (directory, owner, room, now)
    }

    pub(super) fn request(
        authority: WorkspaceTaskAuthority,
        command: WorkspaceTaskCommand,
    ) -> OwnerCommitRequest {
        OwnerCommitRequest {
            room: ROOM.into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: uuid::Uuid::new_v4(),
                authority,
                command,
            },
        }
    }

    pub(super) fn queue(title: &str, instruction: &str) -> WorkspaceTaskCommand {
        WorkspaceTaskCommand::QueueInstruction {
            title: title.into(),
            instruction: instruction.into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
        }
    }

    #[test]
    fn committed_mailbox_reads_authorize_without_waiting_for_mutation() {
        let (_directory, owner, room, now) = enrolled();
        let (person, host) = owner
            .with_current(room, |view| {
                Ok((
                    WorkspaceTaskAuthority::Person {
                        caller: view.device().clone(),
                    },
                    WorkspaceTaskAuthority::Host {
                        attestor: view.host().unwrap().clone(),
                    },
                ))
            })
            .unwrap();
        owner
            .commit_workspace_task_command(
                request(person.clone(), queue("Review", "private goal")),
                now,
            )
            .unwrap();
        let task_id = owner.workspace_task_snapshot(room).unwrap().tasks[0]
            .id
            .clone();
        let reader = owner.clone();
        let (send, receive) = std::sync::mpsc::channel();
        // A durable owner write holds this turn while its candidate is not yet
        // published. Read-only control must still observe the committed view.
        let turn = owner.0.mutation.lock();
        let thread = std::thread::spawn(move || {
            let allowed = reader.task_messages(room, &task_id, 0, &person, false);
            let denied = reader.task_messages(room, &task_id, 0, &host, false);
            send.send((allowed, denied)).unwrap();
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        // Release before joining even on failure so the regression cannot hang.
        drop(turn);
        thread.join().unwrap();
        let (allowed, denied) = result.expect("committed mailbox read waited for mutation");
        assert!(allowed.unwrap().is_empty());
        assert!(denied.unwrap_err().contains("host-only authority"));
    }

    fn task_authorities(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
    ) -> (WorkspaceTaskAuthority, WorkspaceTaskAuthority) {
        let (person, host) = owner
            .with_current(room, |view| {
                Ok((
                    WorkspaceTaskAuthority::Person {
                        caller: view.device().clone(),
                    },
                    WorkspaceTaskAuthority::Host {
                        attestor: view.host().unwrap().clone(),
                    },
                ))
            })
            .unwrap();
        (person, host)
    }

    fn assert_unbound_tasks_cannot_start(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        now: SystemTime,
        host: &WorkspaceTaskAuthority,
    ) -> tilo_peer_state::workspace_task::WorkspaceTaskSnapshot {
        let state = owner.workspace_task_snapshot(room).unwrap();
        for task in &state.tasks {
            assert!(task.assigned_agent.is_none());
            assert_eq!(task.route, WorkspaceTaskRoute::Model);
            assert_eq!(task.status, tilo_desktop_model::WorkspaceTaskStatus::Queued);
            assert!(state.controls[&task.id].active_run.is_none());
            for agent_id in ["", "scribe"] {
                assert!(
                    owner
                        .commit_workspace_task_command(
                            request(
                                host.clone(),
                                WorkspaceTaskCommand::StartRun {
                                    task_id: task.id.clone(),
                                    agent_id: agent_id.into(),
                                }
                            ),
                            now
                        )
                        .is_err()
                );
            }
        }
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), state);
        state
    }

    fn check_private_direction_retry(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        now: SystemTime,
        person: &WorkspaceTaskAuthority,
        child_id: &str,
    ) -> &'static str {
        let direction = "Private child follow-up direction";
        let directed = request(
            person.clone(),
            WorkspaceTaskCommand::DirectionForRun {
                task_id: child_id.to_owned(),
                run_epoch: 1,
                after_revision: 0,
                text: direction.into(),
            },
        );
        owner
            .commit_workspace_task_command(directed.clone(), now)
            .unwrap();
        let accepted = owner
            .with_current(room, |view| {
                Ok((
                    view.generation(),
                    serde_json::to_value(&view.record.private_task_text).unwrap(),
                ))
            })
            .unwrap();
        owner.commit_workspace_task_command(directed, now).unwrap();
        // A stale frontier must not replace the accepted revision's private text.
        assert!(
            owner
                .commit_workspace_task_command(
                    request(
                        person.clone(),
                        WorkspaceTaskCommand::DirectionForRun {
                            task_id: child_id.to_owned(),
                            run_epoch: 1,
                            after_revision: 0,
                            text: "Rejected replacement direction".into(),
                        },
                    ),
                    now,
                )
                .is_err()
        );
        owner
            .with_current(room, |view| {
                assert_eq!(view.generation(), accepted.0);
                assert_eq!(
                    serde_json::to_value(&view.record.private_task_text).unwrap(),
                    accepted.1
                );
                Ok(())
            })
            .unwrap();
        direction
    }

    fn assert_private_child_projection(
        view: &crate::NativeCommittedView,
        child_id: &str,
        goal: &str,
        direction: &str,
    ) {
        let mut doc = view.peer.admitted_room().unwrap().clone();
        let canonical = tilo_peer_state::workspace_task::snapshot(&doc).unwrap();
        assert_eq!(canonical.tasks.len(), 2);
        assert_eq!(
            canonical
                .tasks
                .iter()
                .find(|task| task.id == child_id)
                .unwrap()
                .title,
            "Child"
        );
        assert!(canonical.controls[child_id].instruction.is_none());
        assert_eq!(
            canonical.controls[child_id]
                .private_instruction_ref
                .as_deref(),
            Some(tilo_peer_state::workspace_task::private_instruction_reference(goal).as_str())
        );
        assert_eq!(canonical.controls[child_id].directions.len(), 1);
        assert_eq!(
            canonical.controls[child_id].directions[0].text,
            tilo_peer_state::workspace_task::private_instruction_reference(direction)
        );
        for change in doc.get_changes(&[]) {
            for secret in [goal, direction] {
                assert!(
                    !change
                        .raw_bytes()
                        .windows(secret.len())
                        .any(|bytes| bytes == secret.as_bytes())
                );
            }
        }
    }

    fn mailbox_runs(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        now: SystemTime,
    ) -> (
        WorkspaceTaskAuthority,
        WorkspaceTaskAuthority,
        WorkspaceTaskAuthority,
        tilo_actions::task_messaging::TaskRunAddress,
        tilo_actions::task_messaging::TaskRunAddress,
    ) {
        use tilo_actions::task_messaging::TaskRunAddress;
        let (person, host, agent) = owner
            .with_current(room, |view| {
                Ok((
                    WorkspaceTaskAuthority::Person {
                        caller: view.device().clone(),
                    },
                    WorkspaceTaskAuthority::Host {
                        attestor: view.host().unwrap().clone(),
                    },
                    WorkspaceTaskAuthority::Agent {
                        attestor: view.host().unwrap().clone(),
                        agent_id: "scribe".into(),
                    },
                ))
            })
            .unwrap();
        for title in ["Sender", "Recipient"] {
            owner
                .commit_workspace_task_command(
                    request(person.clone(), queue(title, "private goal")),
                    now,
                )
                .unwrap();
        }
        let tasks = owner.workspace_task_snapshot(room).unwrap().tasks;
        for task in &tasks {
            owner
                .commit_workspace_task_command(
                    request(
                        host.clone(),
                        WorkspaceTaskCommand::StartRun {
                            task_id: task.id.clone(),
                            agent_id: "scribe".into(),
                        },
                    ),
                    now,
                )
                .unwrap();
        }
        let source = TaskRunAddress {
            task_id: tasks[0].id.clone(),
            run_epoch: 1,
            agent_id: "scribe".into(),
        };
        let recipient = TaskRunAddress {
            task_id: tasks[1].id.clone(),
            run_epoch: 1,
            agent_id: "scribe".into(),
        };
        (person, host, agent, source, recipient)
    }

    fn send_idempotent_private_mail(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        source: tilo_actions::task_messaging::TaskRunAddress,
        recipient: &tilo_actions::task_messaging::TaskRunAddress,
        body: &str,
    ) -> tilo_actions::task_messaging::TaskMessage {
        use tilo_actions::task_messaging::TaskMessageSend;
        let invocation = uuid::Uuid::new_v4();
        let send = TaskMessageSend {
            run_epoch: 1,
            text: body.into(),
        };
        let message = owner
            .send_task_message(
                room,
                source.clone(),
                &recipient.task_id,
                invocation,
                send.clone(),
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(message.text, body);
        assert_eq!(
            owner
                .send_task_message(
                    room,
                    source.clone(),
                    &recipient.task_id,
                    invocation,
                    send,
                    |_| Ok(())
                )
                .unwrap(),
            message
        );
        assert!(
            owner
                .send_task_message(
                    room,
                    source,
                    &recipient.task_id,
                    invocation,
                    TaskMessageSend {
                        run_epoch: 1,
                        text: "changed".into()
                    },
                    |_| Ok(())
                )
                .is_err()
        );
        message
    }

    #[test]
    fn private_mail_and_output_receipts_survive_reopen_without_public_hydration() {
        use tilo_actions::task_messaging::{TaskOutputRef, TaskOutputStatus};
        let (directory, owner, room, now) = enrolled();
        let (person, host, agent, source, recipient) = mailbox_runs(&owner, room, now);
        let body = "  private mailbox canary\nexact body  ";
        let message = send_idempotent_private_mail(&owner, room, source, &recipient, body);
        let output = TaskOutputRef {
            version: 1,
            task_run: recipient.clone(),
            invocation: uuid::Uuid::new_v4().to_string(),
            provider: "private-output-provider".into(),
            resource: "private-output-canary".into(),
            open_action: "open".into(),
            durable_proof: "a".repeat(64),
            status: TaskOutputStatus::Created,
        };
        owner
            .record_task_outputs(room, &recipient, vec![output.clone()], |_| Ok(()))
            .unwrap();
        owner
            .acknowledge_task_messages(
                room,
                &recipient.task_id,
                1,
                &agent,
                std::slice::from_ref(&message.message_id),
            )
            .unwrap();
        assert!(
            owner
                .task_messages(room, &recipient.task_id, 1, &agent, true)
                .unwrap()
                .is_empty()
        );
        assert!(
            owner
                .task_messages(room, &recipient.task_id, 2, &agent, false)
                .is_err()
        );
        assert!(
            owner
                .task_outputs(room, &recipient.task_id, 1, &host)
                .is_err()
        );
        owner
            .ensure_private_task_baseline(room, &SigningKey::from_bytes(&[61; 32]), now)
            .unwrap();
        let snapshot = format!("{:?}", owner.workspace_task_snapshot(room).unwrap());
        assert!(!snapshot.contains("private-output-canary"));
        owner
            .with_current(room, |view| {
                let public = view.peer().durable()?;
                assert!(
                    !public
                        .windows(body.len())
                        .any(|bytes| bytes == body.as_bytes())
                );
                assert!(!format!("{:?}", view.record.private_task_mailboxes).contains(body));
                Ok(())
            })
            .unwrap();
        drop(owner);
        let owner = crate::open_file(&directory.path().join("rooms.json")).unwrap();
        assert_eq!(
            owner
                .task_messages(room, &recipient.task_id, 1, &person, false)
                .unwrap(),
            vec![message]
        );
        assert_eq!(
            owner
                .task_outputs(room, &recipient.task_id, 1, &person)
                .unwrap(),
            vec![output]
        );
        assert!(
            owner
                .task_messages(room, &recipient.task_id, 1, &agent, true)
                .unwrap()
                .is_empty()
        );
    }

    fn start_report_run(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        now: SystemTime,
    ) -> (
        WorkspaceTaskAuthority,
        WorkspaceTaskAuthority,
        WorkspaceTaskAuthority,
        String,
    ) {
        let (person, host) = owner
            .with_current(room, |view| {
                Ok((
                    WorkspaceTaskAuthority::Person {
                        caller: view.device().clone(),
                    },
                    WorkspaceTaskAuthority::Host {
                        attestor: view.host().unwrap().clone(),
                    },
                ))
            })
            .unwrap();
        owner
            .commit_workspace_task_command(
                request(person.clone(), queue("Review", "private goal")),
                now,
            )
            .unwrap();
        let id = owner.workspace_task_snapshot(room).unwrap().tasks[0]
            .id
            .clone();
        owner
            .commit_workspace_task_command(
                request(
                    host.clone(),
                    WorkspaceTaskCommand::StartRun {
                        task_id: id.clone(),
                        agent_id: "scribe".into(),
                    },
                ),
                now,
            )
            .unwrap();
        let WorkspaceTaskAuthority::Host { attestor } = &host else {
            panic!("expected host authority")
        };
        let agent = WorkspaceTaskAuthority::Agent {
            attestor: attestor.clone(),
            agent_id: "scribe".into(),
        };
        owner
            .commit_workspace_task_command(
                request(
                    agent.clone(),
                    WorkspaceTaskCommand::ExecutionPhase {
                        task_id: id.clone(),
                        run_epoch: 1,
                        sequence: 0,
                        phase: tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted,
                    },
                ),
                now,
            )
            .unwrap();
        (person, host, agent, id)
    }

    fn publish_private_progress(
        owner: &NativeWorkspaceOwner,
        now: SystemTime,
        agent: &WorkspaceTaskAuthority,
        id: &str,
        progress: &str,
    ) -> (uuid::Uuid, u64) {
        let update = request(
            agent.clone(),
            WorkspaceTaskCommand::PublicProgress {
                task_id: id.to_owned(),
                run_epoch: 1,
                sequence: 1,
                message: progress.into(),
            },
        );
        let progress_invocation = match &update.operation {
            tilo_owner_commit::CommitOperation::WorkspaceTaskCommand { invocation, .. } => {
                *invocation
            }
            _ => panic!("expected workspace task command"),
        };
        let accepted_after = crate::enrollment::ns(SystemTime::now()).unwrap();
        owner
            .commit_workspace_task_command(update.clone(), now)
            .unwrap();
        owner
            .commit_workspace_task_command(update.clone(), now)
            .unwrap();
        let mut changed_body = update;
        if let tilo_owner_commit::CommitOperation::WorkspaceTaskCommand {
            command: WorkspaceTaskCommand::PublicProgress { message, .. },
            ..
        } = &mut changed_body.operation
        {
            *message = "different private body".into();
        }
        assert!(
            owner
                .commit_workspace_task_command(changed_body, now)
                .is_err()
        );
        (progress_invocation, accepted_after)
    }

    fn publish_bounded_private_presentation(
        owner: &NativeWorkspaceOwner,
        now: SystemTime,
        agent: &WorkspaceTaskAuthority,
        id: &str,
        path: &std::path::Path,
    ) -> collab_types::task_presentation::TaskPresentation {
        let presentation = collab_types::task_presentation::TaskPresentation {
            run: "private-turn:1".into(),
            sequence: 3,
            received_at: Some(1234),
            text: format!("  exact presentation answer\n{}", "é".repeat(3058)),
            reasoning: "é".repeat(512),
            finish: Some(collab_types::task_presentation::TaskPresentationFinish::Complete),
            detail: "é".repeat(512),
        };
        assert_eq!(
            presentation
                .text
                .len()
                .checked_add(presentation.reasoning.len())
                .unwrap()
                .checked_add(presentation.detail.len())
                .unwrap(),
            tilo_actions::task_messaging::TaskReportPresentation::MAX_PROSE_BYTES
        );
        let presentation_request = request(
            agent.clone(),
            WorkspaceTaskCommand::Presentation {
                task_id: id.to_owned(),
                run_epoch: 1,
                sequence: 2,
                presentation: presentation.clone(),
            },
        );
        owner
            .commit_workspace_task_command(presentation_request.clone(), now)
            .unwrap();
        owner
            .commit_workspace_task_command(presentation_request, now)
            .unwrap();
        for field in ["text", "reasoning", "detail"] {
            let mut oversized = presentation.clone();
            oversized.text = "é".repeat(3072);
            oversized.reasoning = "é".repeat(512);
            oversized.detail = "é".repeat(512);
            match field {
                "text" => oversized.text.push('x'),
                "reasoning" => oversized.reasoning.push('x'),
                _ => oversized.detail.push('x'),
            }
            let before = std::fs::read(path.join("rooms.json")).unwrap();
            assert!(
                owner
                    .commit_workspace_task_command(
                        request(
                            agent.clone(),
                            WorkspaceTaskCommand::Presentation {
                                task_id: id.to_owned(),
                                run_epoch: 1,
                                sequence: 3,
                                presentation: oversized,
                            },
                        ),
                        now
                    )
                    .is_err(),
                "{field}"
            );
            assert_eq!(std::fs::read(path.join("rooms.json")).unwrap(), before);
        }
        presentation
    }

    struct ReportChecks<'a> {
        id: &'a str,
        progress: &'a str,
        terminal: &'a str,
        presentation: &'a collab_types::task_presentation::TaskPresentation,
        progress_invocation: uuid::Uuid,
        attestor: &'a tilo_user_identity::DeviceCert,
        accepted: std::ops::RangeInclusive<u64>,
        fail: bool,
    }
    impl ReportChecks<'_> {
        fn assert_reopened(
            &self,
            reopened: &NativeWorkspaceOwner,
            room: RoomId,
            person: &WorkspaceTaskAuthority,
            host: &WorkspaceTaskAuthority,
        ) {
            let id = self.id;
            let progress = self.progress;
            let terminal = self.terminal;
            let presentation = self.presentation;
            let progress_invocation = self.progress_invocation;
            let attestor = self.attestor;
            let accepted_after = *self.accepted.start();
            let accepted_before = *self.accepted.end();
            let reports = reopened
                .task_report_transcript(room, id, 1, person)
                .unwrap();
            assert_eq!(reports.len(), 3);
            let exact = reports
                .iter()
                .find(|report| report.presentation.is_some())
                .unwrap();
            assert_eq!(exact.body, presentation.text);
            assert_eq!(
                serde_json::to_value(exact.presentation.as_ref().unwrap()).unwrap(),
                serde_json::to_value(presentation).unwrap()
            );
            assert!(reports.iter().any(|report| report.body == terminal));
            let progress_report = reports
                .iter()
                .find(|report| report.body == progress)
                .unwrap();
            assert_eq!(progress_report.invocation, progress_invocation);
            assert_eq!(progress_report.source_device, attestor.device_public_key);
            assert_eq!(
                progress_report.role,
                crate::NativePrivateTaskReportRole::Assistant
            );
            assert!(
                (accepted_after..=accepted_before).contains(&progress_report.accepted_at_unix_ns)
            );
            assert!(reopened.task_report_transcript(room, id, 1, host).is_err());
        }
        fn assert_committed(&self, view: &crate::NativeCommittedView) {
            let id = self.id;
            let progress = self.progress;
            let terminal = self.terminal;
            let presentation = self.presentation;
            let progress_invocation = self.progress_invocation;
            let attestor = self.attestor;
            let accepted_after = *self.accepted.start();
            let accepted_before = *self.accepted.end();
            let fail = self.fail;
            let reports = &view.record.private_task_text.reports;
            assert_eq!(reports.len(), 3);
            let exact = reports
                .values()
                .find(|report| report.presentation.is_some())
                .unwrap();
            assert_eq!(exact.body, presentation.text);
            assert_eq!(
                serde_json::to_value(exact.presentation.as_ref().unwrap()).unwrap(),
                serde_json::to_value(presentation).unwrap()
            );
            assert_eq!(reports[&progress_invocation.to_string()].body, progress);
            assert!(reports.values().any(|report| report.body == progress));
            assert!(reports.values().any(|report| report.body == terminal));
            for report in reports.values() {
                assert_eq!(report.task_run.task_id, id);
                assert_eq!(report.task_run.run_epoch, 1);
                assert_eq!(report.task_run.agent_id, "scribe");
                assert_eq!(report.source_device, attestor.device_public_key);
                assert_eq!(report.role, crate::NativePrivateTaskReportRole::Assistant);
                assert!((accepted_after..=accepted_before).contains(&report.accepted_at_unix_ns));
                assert_eq!(
                    report.actor,
                    if fail && report.body == terminal {
                        "host"
                    } else {
                        "agent:scribe"
                    }
                );
            }
            let serialized_public = serde_json::to_string(
                &tilo_peer_state::workspace_task::snapshot(view.peer.admitted_room().unwrap())
                    .unwrap(),
            )
            .unwrap();
            assert!(!serialized_public.contains("private progress canary"));
            assert!(!serialized_public.contains("private terminal canary"));
            assert!(!serialized_public.contains("exact presentation answer"));
            assert!(!serialized_public.contains("private presentation reasoning"));
            assert!(!serialized_public.contains("private presentation detail"));
            assert!(!serialized_public.contains("Agent run"));
            let mut doc = view.peer.admitted_room().unwrap().clone();
            for secret in [
                progress,
                terminal,
                presentation.text.as_str(),
                presentation.reasoning.as_str(),
                presentation.detail.as_str(),
            ] {
                for change in doc.get_changes(&[]) {
                    assert!(
                        !change
                            .raw_bytes()
                            .windows(secret.len())
                            .any(|bytes| bytes == secret.as_bytes())
                    );
                }
                for publication in crate::testing::entries(view).unwrap() {
                    assert!(
                        !publication
                            .windows(secret.len())
                            .any(|bytes| bytes == secret.as_bytes())
                    );
                }
            }
        }
    }

    #[test]
    fn automatic_reports_keep_exact_private_bodies_out_of_public_history() {
        for fail in [false, true] {
            let (directory, owner, room, now) = enrolled();
            let (person, host, agent, id) = start_report_run(&owner, room, now);
            let WorkspaceTaskAuthority::Host { attestor } = &host else {
                panic!("expected host authority")
            };
            let progress = "  private progress canary\nwith exact whitespace  ";
            let terminal = "  private terminal canary\nwith exact whitespace  ";
            let (progress_invocation, accepted_after) =
                publish_private_progress(&owner, now, &agent, &id, progress);
            let presentation =
                publish_bounded_private_presentation(&owner, now, &agent, &id, directory.path());
            // Rejected stale reports must not leave a private transcript entry.
            assert!(
                owner
                    .commit_workspace_task_command(
                        request(
                            agent.clone(),
                            WorkspaceTaskCommand::PublicProgress {
                                task_id: id.clone(),
                                run_epoch: 2,
                                sequence: 2,
                                message: "rejected private canary".into(),
                            }
                        ),
                        now
                    )
                    .is_err()
            );
            let terminal_request = if fail {
                request(
                    host.clone(),
                    WorkspaceTaskCommand::Abandon {
                        task_id: id.clone(),
                        run_epoch: 1,
                        reason: terminal.into(),
                    },
                )
            } else {
                request(
                    agent,
                    WorkspaceTaskCommand::Finish {
                        task_id: id.clone(),
                        run_epoch: 1,
                        outcome: tilo_peer_state::workspace_task::WorkspaceTaskFinish::Done,
                        summary: terminal.into(),
                    },
                )
            };
            owner
                .commit_workspace_task_command(terminal_request.clone(), now)
                .unwrap();
            owner
                .commit_workspace_task_command(terminal_request, now)
                .unwrap();
            let accepted_before = crate::enrollment::ns(SystemTime::now()).unwrap();
            let checks = ReportChecks {
                id: &id,
                progress,
                terminal,
                presentation: &presentation,
                progress_invocation,
                attestor,
                accepted: accepted_after..=accepted_before,
                fail,
            };
            owner
                .with_current(room, |view| {
                    checks.assert_committed(view);
                    Ok(())
                })
                .unwrap();
            let public =
                serde_json::to_string(&owner.workspace_task_snapshot(room).unwrap()).unwrap();
            assert!(!public.contains("private progress canary"));
            assert!(!public.contains("private terminal canary"));
            drop(owner);
            let reopened = crate::open_file(&directory.path().join("rooms.json")).unwrap();
            checks.assert_reopened(&reopened, room, &person, &host);
        }
    }

    #[test]
    fn one_line_private_instruction_cannot_be_republished_as_an_automatic_title() {
        let (_directory, owner, room, now) = enrolled();
        let canary = "one-line-owner-private-canary";
        let authority = owner
            .with_current(room, |view| {
                Ok(WorkspaceTaskAuthority::Person {
                    caller: view.device().clone(),
                })
            })
            .unwrap();
        owner
            .commit_workspace_task_command_with_key(
                request(authority, queue(canary, canary)),
                &SigningKey::from_bytes(&[61; 32]),
                now,
            )
            .unwrap();
        owner
            .with_current(room, |view| {
                let mut doc = view.peer.admitted_room().unwrap().clone();
                for change in doc.get_changes(&[]) {
                    assert!(
                        !change
                            .raw_bytes()
                            .windows(canary.len())
                            .any(|bytes| bytes == canary.as_bytes())
                    );
                }
                for publication in crate::testing::entries(view)? {
                    assert!(
                        !publication
                            .windows(canary.len())
                            .any(|bytes| bytes == canary.as_bytes())
                    );
                }
                assert!(
                    serde_json::to_string(&view.record.private_task_text)
                        .unwrap()
                        .contains(canary)
                );
                Ok(())
            })
            .unwrap();
        assert!(
            !serde_json::to_string(&owner.workspace_task_snapshot(room).unwrap().tasks)
                .unwrap()
                .contains(canary)
        );
    }

    #[test]
    fn selected_task_keeps_its_subject_without_publishing_private_instruction() {
        let (_directory, owner, room, now) = enrolled();
        let canary = "selected-task-owner-private-canary";
        let subject = tilo_actions::SelectionSubject {
            app_id: "so.tilo.word-processor".into(),
            app_label: "Writer".into(),
            resource: tilo_actions::ActionResource {
                id: "doc:proposal".into(),
                kind: "word-processor.document".into(),
                label: "Proposal".into(),
            },
            anchor: tilo_actions::SelectionAnchor::of_text("opening sentence"),
        };
        let authority = owner
            .with_current(room, |view| {
                Ok(WorkspaceTaskAuthority::Person {
                    caller: view.device().clone(),
                })
            })
            .unwrap();
        owner
            .commit_workspace_task_command_with_key(
                request(
                    authority,
                    WorkspaceTaskCommand::QueueInstructionAbout {
                        title: canary.into(),
                        instruction: canary.into(),
                        assigned_agent: "scribe".into(),
                        route: WorkspaceTaskRoute::Model,
                        subject: Box::new(subject.clone()),
                    },
                ),
                &SigningKey::from_bytes(&[61; 32]),
                now,
            )
            .unwrap();
        let snapshot = owner.workspace_task_snapshot(room).unwrap();
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.tasks[0].subject.as_ref(), Some(&subject));
        assert!(snapshot.tasks[0].resource.is_none());
        assert_eq!(snapshot.tasks[0].title, "Agent task");
        assert!(!serde_json::to_string(&snapshot).unwrap().contains(canary));
        owner
            .with_current(room, |view| {
                let mut doc = view.peer.admitted_room().unwrap().clone();
                for change in doc.get_changes(&[]) {
                    assert!(
                        !change
                            .raw_bytes()
                            .windows(canary.len())
                            .any(|bytes| bytes == canary.as_bytes())
                    );
                }
                for publication in crate::testing::entries(view)? {
                    assert!(
                        !publication
                            .windows(canary.len())
                            .any(|bytes| bytes == canary.as_bytes())
                    );
                }
                assert_eq!(
                    view.record.private_task_text.instructions()[&snapshot.tasks[0].id]
                        .instruction
                        .as_deref(),
                    Some(canary)
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn rules_only_directions_are_prepared_as_private_before_reduction() {
        let (_directory, owner, room, now) = enrolled();
        let command = WorkspaceTaskCommand::QueueInstruction {
            title: "Rules task".into(),
            instruction: "owner-only rules goal".into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::RulesOnly,
        };
        let authority = owner
            .with_current(room, |view| {
                Ok(WorkspaceTaskAuthority::Person {
                    caller: view.device().clone(),
                })
            })
            .unwrap();
        owner
            .commit_workspace_task_command(request(authority, command), now)
            .unwrap();
        owner
            .with_current(room, |view| {
                let state =
                    tilo_peer_state::workspace_task::snapshot(view.peer.admitted_room().unwrap())?;
                let task_id = state.tasks.last().unwrap().id.clone();
                let (public, write) = view.record.private_task_text.prepare_private_command(
                    &WorkspaceTaskCommand::Direction {
                        task_id,
                        text: "rules private canary".into(),
                    },
                    &view.peer,
                )?;
                let WorkspaceTaskCommand::Direction { text, .. } = public else {
                    panic!("direction changed variant")
                };
                assert_eq!(
                    text,
                    tilo_peer_state::workspace_task::private_instruction_reference(
                        "rules private canary"
                    )
                );
                assert!(write.is_some());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn unbound_private_model_tasks_survive_reopen_with_goals_and_labels() {
        let (directory, owner, room, now) = enrolled();
        let (person, host) = task_authorities(&owner, room);
        let goal = "Retain this private model goal until an executor is configured";
        owner
            .commit_workspace_task_command(
                request(
                    person.clone(),
                    WorkspaceTaskCommand::QueueInstruction {
                        title: "Research".into(),
                        instruction: goal.into(),
                        assigned_agent: String::new(),
                        route: WorkspaceTaskRoute::Model,
                    },
                ),
                now,
            )
            .unwrap();
        let parent = owner.workspace_task_snapshot(room).unwrap().tasks[0]
            .id
            .clone();
        let child_goal = "Private follow-up goal";
        let queued = request(
            person.clone(),
            WorkspaceTaskCommand::QueuePrivateSubtask {
                parent_task_id: parent.clone(),
                title: "Follow-up".into(),
                instruction: child_goal.into(),
                assigned_agent: String::new(),
                route: WorkspaceTaskRoute::Model,
            },
        );
        owner
            .commit_workspace_task_command(queued.clone(), now)
            .unwrap();
        owner.commit_workspace_task_command(queued, now).unwrap();
        let state = owner.workspace_task_snapshot(room).unwrap();
        assert_eq!(state.tasks.len(), 2);
        let child = state
            .tasks
            .iter()
            .find(|task| task.id != parent)
            .unwrap()
            .id
            .clone();
        owner
            .commit_workspace_task_command(
                request(
                    person.clone(),
                    WorkspaceTaskCommand::SetLabels {
                        task_id: child.clone(),
                        labels: vec!["research".into()],
                    },
                ),
                now,
            )
            .unwrap();
        let state = assert_unbound_tasks_cannot_start(&owner, room, now, &host);
        drop(owner);
        let reopened = crate::open_file(&directory.path().join("rooms.json")).unwrap();
        assert_eq!(reopened.workspace_task_snapshot(room).unwrap(), state);
        let child_task = state.tasks.iter().find(|task| task.id == child).unwrap();
        assert_eq!(child_task.parent_task_id.as_deref(), Some(parent.as_str()));
        assert_eq!(child_task.labels, ["research"]);
        assert_eq!(child_task.title, "Follow-up");
        for (task_id, instruction) in [(&parent, goal), (&child, child_goal)] {
            assert_eq!(
                reopened
                    .workspace_task_private_instructions(room, task_id, 0, &person, now)
                    .unwrap()
                    .unwrap()
                    .instruction
                    .as_deref(),
                Some(instruction)
            );
        }
        reopened
            .with_current(room, |view| {
                let mut doc = view.peer.admitted_room().unwrap().clone();
                for change in doc.get_changes(&[]) {
                    for secret in [goal, child_goal] {
                        assert!(
                            !change
                                .raw_bytes()
                                .windows(secret.len())
                                .any(|bytes| bytes == secret.as_bytes())
                        );
                    }
                }
                Ok(())
            })
            .unwrap();
    }

    fn reject_invalid_private_subtasks(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        now: SystemTime,
        person: &WorkspaceTaskAuthority,
        host: &WorkspaceTaskAuthority,
        parent: &str,
        goal: &str,
    ) {
        let before = owner.workspace_task_snapshot(room).unwrap();
        let child_command = |parent_task_id| WorkspaceTaskCommand::QueuePrivateSubtask {
            parent_task_id,
            title: "Child".into(),
            instruction: goal.into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
        };
        let retained_before = owner
            .with_current(room, |view| {
                Ok((
                    view.generation(),
                    serde_json::to_value(&view.record.private_task_text).unwrap(),
                ))
            })
            .unwrap();
        for (authority, parent_id) in [
            (person.clone(), "task:missing".to_owned()),
            (host.clone(), parent.to_owned()),
        ] {
            assert!(
                owner
                    .commit_workspace_task_command(
                        request(authority, child_command(parent_id)),
                        now
                    )
                    .is_err()
            );
            assert_eq!(owner.workspace_task_snapshot(room).unwrap(), before);
            owner
                .with_current(room, |view| {
                    assert_eq!(view.generation(), retained_before.0);
                    assert_eq!(
                        serde_json::to_value(&view.record.private_task_text).unwrap(),
                        retained_before.1
                    );
                    Ok(())
                })
                .unwrap();
        }
    }

    #[test]
    fn private_subtask_is_atomic_and_never_writes_its_goal_into_the_room() {
        let (_directory, owner, room, now) = enrolled();
        let (person, host) = task_authorities(&owner, room);
        owner
            .commit_workspace_task_command(
                request(
                    person.clone(),
                    WorkspaceTaskCommand::CreateManual {
                        title: "Parent".into(),
                        resource: None,
                    },
                ),
                now,
            )
            .unwrap();
        let before = owner.workspace_task_snapshot(room).unwrap();
        let parent = before.tasks[0].id.clone();
        let goal = "Private child goal, not the public title";
        let child_command = |parent_task_id| WorkspaceTaskCommand::QueuePrivateSubtask {
            parent_task_id,
            title: "Child".into(),
            instruction: goal.into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
        };
        reject_invalid_private_subtasks(&owner, room, now, &person, &host, &parent, goal);
        let queued = request(person.clone(), child_command(parent.clone()));
        owner
            .commit_workspace_task_command(queued.clone(), now)
            .unwrap();
        let public = owner.workspace_task_snapshot(room).unwrap();
        owner.commit_workspace_task_command(queued, now).unwrap();
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), public);
        assert_eq!(public.tasks.len(), 2);
        let child = public.tasks.iter().find(|task| task.id != parent).unwrap();
        assert_eq!(child.parent_task_id.as_deref(), Some(parent.as_str()));
        assert!(public.controls[&child.id].instruction.is_none());
        assert_eq!(
            public.controls[&child.id]
                .private_instruction_ref
                .as_deref(),
            Some(tilo_peer_state::workspace_task::private_instruction_reference(goal).as_str())
        );
        assert!(public.controls[&child.id].active_run.is_none());
        assert_eq!(
            owner
                .workspace_task_private_instructions(room, &child.id, 0, &person, now)
                .unwrap()
                .unwrap()
                .instruction
                .as_deref(),
            Some(goal)
        );
        owner
            .commit_workspace_task_command(
                request(
                    host,
                    WorkspaceTaskCommand::StartRun {
                        task_id: child.id.clone(),
                        agent_id: "scribe".into(),
                    },
                ),
                now,
            )
            .unwrap();
        let direction = check_private_direction_retry(&owner, room, now, &person, &child.id);
        let private = owner
            .workspace_task_private_instructions(room, &child.id, 1, &person, now)
            .unwrap()
            .unwrap();
        assert_eq!(private.instruction.as_deref(), Some(goal));
        assert_eq!(private.directions[&1], direction);
        owner
            .with_current(room, |view| {
                assert_private_child_projection(view, &child.id, goal, direction);
                Ok(())
            })
            .unwrap();
    }

    fn start_private_review(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        now: SystemTime,
        person: &WorkspaceTaskAuthority,
        host: &WorkspaceTaskAuthority,
    ) -> (String, WorkspaceTaskAuthority) {
        let queued = request(
            person.clone(),
            queue("Review", "private initial instruction"),
        );
        owner
            .commit_workspace_task_command(queued.clone(), now)
            .unwrap();
        owner.commit_workspace_task_command(queued, now).unwrap();
        let public = owner.workspace_task_snapshot(room).unwrap();
        assert_eq!(public.tasks.len(), 1);
        let id = public.tasks[0].id.clone();
        assert!(public.controls[&id].instruction.is_none());
        assert_eq!(
            owner
                .workspace_task_private_instructions(room, &id, 0, person, now)
                .unwrap()
                .unwrap()
                .instruction
                .as_deref(),
            Some("private initial instruction")
        );
        assert!(
            owner
                .workspace_task_private_instructions(room, &id, 0, host, now)
                .is_err()
        );
        let WorkspaceTaskAuthority::Host { attestor } = host else {
            panic!("expected a host authority")
        };
        let agent = WorkspaceTaskAuthority::Agent {
            attestor: attestor.clone(),
            agent_id: "scribe".into(),
        };
        assert!(
            owner
                .workspace_task_private_instructions(room, &id, 0, &agent, now)
                .is_err()
        );
        owner
            .commit_workspace_task_command(
                request(
                    host.clone(),
                    WorkspaceTaskCommand::StartRun {
                        task_id: id.clone(),
                        agent_id: "scribe".into(),
                    },
                ),
                now,
            )
            .unwrap();
        (id, agent)
    }

    #[test]
    fn private_text_is_exact_run_scoped_and_survives_reopen_without_public_hydration() {
        let (directory, owner, room, now) = enrolled();
        let (person, host) = task_authorities(&owner, room);
        let (id, agent) = start_private_review(&owner, room, now, &person, &host);
        owner
            .commit_workspace_task_command(
                request(
                    person.clone(),
                    WorkspaceTaskCommand::DirectionForRun {
                        task_id: id.clone(),
                        run_epoch: 1,
                        after_revision: 0,
                        text: "private next direction".into(),
                    },
                ),
                now,
            )
            .unwrap();
        assert!(
            owner
                .workspace_task_private_instructions(room, &id, 0, &agent, now)
                .is_err()
        );
        // A person's retained task custody can inspect the original epoch;
        // the same epoch remains unavailable to the current model actor.
        assert_eq!(
            owner
                .workspace_task_private_instructions(room, &id, 0, &person, now)
                .unwrap()
                .unwrap()
                .instruction
                .as_deref(),
            Some("private initial instruction")
        );
        let instructions = owner
            .workspace_task_private_instructions(room, &id, 1, &agent, now)
            .unwrap()
            .unwrap();
        assert_eq!(
            instructions.directions.get(&1).map(String::as_str),
            Some("private next direction")
        );
        let public = owner.workspace_task_snapshot(room).unwrap();
        assert_eq!(
            public.controls[&id].directions[0].text,
            tilo_peer_state::workspace_task::private_instruction_reference(
                "private next direction"
            )
        );
        owner
            .with_current(room, |view| {
                let canonical =
                    tilo_peer_state::workspace_task::snapshot(view.peer.admitted_room().unwrap())?;
                assert!(canonical.controls[&id].instruction.is_none());
                assert_eq!(
                    canonical.controls[&id].directions[0].text,
                    public.controls[&id].directions[0].text
                );
                Ok(())
            })
            .unwrap();
        drop(owner);
        let reopened = crate::open_file(&directory.path().join("rooms.json")).unwrap();
        assert_eq!(
            reopened
                .workspace_task_private_instructions(room, &id, 1, &person, now)
                .unwrap()
                .unwrap()
                .directions
                .get(&1)
                .map(String::as_str),
            Some("private next direction")
        );
        // References retained by an earlier keyless command must not be
        // mistaken for new plaintext during the eventual signed migration.
        reopened
            .commit_workspace_task_command_with_key(
                request(person, queue("Another task", "another private instruction")),
                &SigningKey::from_bytes(&[61; 32]),
                now,
            )
            .unwrap();
        assert_eq!(
            reopened.workspace_task_snapshot(room).unwrap().controls[&id].directions[0].text,
            tilo_peer_state::workspace_task::private_instruction_reference(
                "private next direction"
            )
        );
        assert_eq!(
            reopened
                .workspace_task_private_instructions(room, &id, 1, &agent, now)
                .unwrap()
                .unwrap()
                .directions
                .get(&1)
                .map(String::as_str),
            Some("private next direction")
        );
    }

    #[test]
    fn agent_request_binding_rejects_missing_custody_and_changed_request_body() {
        let (_directory, owner, room, now) = enrolled();
        let person = owner
            .with_current(room, |view| {
                Ok(WorkspaceTaskAuthority::Person {
                    caller: view.device().clone(),
                })
            })
            .unwrap();
        let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
        let instruction = "Private source request with an exact body";
        owner
            .commit_workspace_task_command(
                request(
                    person.clone(),
                    WorkspaceTaskCommand::Queue {
                        title: "Request summary".into(),
                        assigned_agent: "scribe".into(),
                        route: WorkspaceTaskRoute::Model,
                        resource: Some(Box::new(tilo_desktop_model::WorkspaceTaskResource {
                            app_id: Some("so.tilo.chat".into()),
                            app_label: Some("Chat".into()),
                            id: format!("chat.message:m_{}", invocation.0),
                            kind: "chat.message".into(),
                            label: "Source message".into(),
                            agent_request: Some(tilo_desktop_model::WorkspaceAgentRequest {
                                chat_record: "record".into(),
                                channel_id: "channel".into(),
                                message_id: format!("m_{}", invocation.0),
                                instruction: instruction.into(),
                                invocation,
                            }),
                        })),
                    },
                ),
                now,
            )
            .unwrap();
        let snapshot = owner.workspace_task_snapshot(room).unwrap();
        let id = snapshot.tasks[0].id.clone();
        assert_eq!(
            snapshot.controls[&id].private_instruction_ref,
            Some(tilo_peer_state::workspace_task::private_instruction_reference(instruction))
        );
        assert!(
            !serde_json::to_string(&(&snapshot.tasks, &snapshot.controls, &snapshot.grants))
                .unwrap()
                .contains(instruction)
        );
        let original = owner.with_current(room, |view| Ok(view.clone())).unwrap();
        assert!(
            owner
                .workspace_task_private_instructions(room, &id, 0, &person, now)
                .unwrap()
                .is_some()
        );
        for missing in [true, false] {
            let mut candidate = original.clone();
            if missing {
                candidate.record.private_task_text.instructions.remove(&id);
            } else {
                candidate
                    .record
                    .private_task_text
                    .instructions
                    .get_mut(&id)
                    .unwrap()
                    .agent_request
                    .as_mut()
                    .unwrap()
                    .instruction = "Replaced request body".into();
            }
            candidate.record.generation = owner
                .with_current(room, |view| Ok(view.generation() + 1))
                .unwrap();
            owner.publish_room(room, candidate, None).unwrap();
            assert!(
                owner
                    .workspace_task_private_instructions(room, &id, 0, &person, now)
                    .is_err()
            );
            assert_eq!(owner.workspace_task_snapshot(room).unwrap(), snapshot);
        }
    }

    #[test]
    fn keyed_migration_keeps_private_text_and_preserves_structural_metadata() {
        let (_directory, owner, room, now) = enrolled();
        let person = owner
            .with_current(room, |view| {
                Ok(WorkspaceTaskAuthority::Person {
                    caller: view.device().clone(),
                })
            })
            .unwrap();
        let legacy = request(
            person.clone(),
            queue("Legacy title", "legacy private instruction"),
        );
        let binding = legacy.binding().unwrap();
        let CommitOperation::WorkspaceTaskCommand {
            invocation,
            authority,
            command,
        } = legacy.operation
        else {
            panic!("legacy queue must retain its task command");
        };
        // Simulate an on-disk room written before private task storage existed.
        let mut candidate = owner.with_current(room, |view| Ok(view.clone())).unwrap();
        let (_, result) = candidate
            .peer
            .workspace_mut()
            .apply_workspace_task_command(
                invocation,
                binding.request_digest,
                &authority,
                &command,
                now,
            )
            .unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = result else {
            panic!("expected a queued task")
        };
        let before =
            tilo_peer_state::workspace_task::snapshot(candidate.peer.admitted_room().unwrap())
                .unwrap()
                .tasks
                .into_iter()
                .find(|task| task.id == task_id)
                .unwrap();
        candidate.record.peer = candidate.peer.durable().unwrap();
        candidate.record.generation += 1;
        owner.publish_room(room, candidate, None).unwrap();
        owner
            .commit_workspace_task_command_with_key(
                request(person.clone(), queue("New task", "new private instruction")),
                &SigningKey::from_bytes(&[61; 32]),
                now,
            )
            .unwrap();
        let after = owner.workspace_task_snapshot(room).unwrap();
        let task = after.tasks.iter().find(|task| task.id == task_id).unwrap();
        assert_eq!(task.parent_task_id, before.parent_task_id);
        assert_eq!(task.child_order, before.child_order);
        assert_eq!(task.labels, before.labels);
        assert_eq!(task.responsible_assignees, before.responsible_assignees);
        assert!(after.controls[&task_id].instruction.is_none());
        assert_eq!(
            owner
                .workspace_task_private_instructions(room, &task_id, 0, &person, now)
                .unwrap()
                .unwrap()
                .instruction
                .as_deref(),
            Some("legacy private instruction")
        );
        owner
            .with_current(room, |view| {
                assert!(view.record.sanitized_baseline.is_some());
                crate::publication::validate(view)?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn rejected_private_text_does_not_publish_or_allocate_a_task() {
        let (_directory, owner, room, now) = enrolled();
        let person = owner
            .with_current(room, |view| {
                Ok(WorkspaceTaskAuthority::Person {
                    caller: view.device().clone(),
                })
            })
            .unwrap();
        let generation = owner
            .with_current(room, |view| Ok(view.generation()))
            .unwrap();
        assert!(
            owner
                .commit_workspace_task_command(request(person, queue("Invalid", "   ")), now,)
                .is_err()
        );
        assert_eq!(
            owner
                .with_current(room, |view| Ok(view.generation()))
                .unwrap(),
            generation
        );
        assert!(
            owner
                .workspace_task_snapshot(room)
                .unwrap()
                .tasks
                .is_empty()
        );
    }
}
