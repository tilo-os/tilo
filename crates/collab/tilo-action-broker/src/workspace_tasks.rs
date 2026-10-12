//! Task coordination and owner-configured execution policy through the existing invocation service.
//! Inputs may select a planning mode and recipient; never actor, custody, status or lease.
use super::{InvocationEnvelope, PublishedAction, PublishedInput};
use tilo_desktop_model::{
    WorkspaceTask, WorkspaceTaskMutationResult, WorkspaceTaskResource, WorkspaceTaskRoute,
};
use tilo_peer_state::workspace_task::{
    WorkspaceTaskCommand, WorkspaceTaskSnapshot, validate_manual_resource,
};

const WORKSPACE: &str = "desktop:workspace";
const TASK_PREFIX: &str = "desktop:task:";
const MAX_ARGUMENT_BYTES: usize = 8192;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceInput {
    app_id: String,
    app_label: String,
    id: String,
    kind: String,
    label: String,
}

fn resource(encoded: &str) -> Result<WorkspaceTaskResource, String> {
    if encoded.len() > MAX_ARGUMENT_BYTES {
        return Err("task resource exceeds bounded input".into());
    }
    let input: ResourceInput = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
    let value = WorkspaceTaskResource {
        app_id: Some(input.app_id),
        app_label: Some(input.app_label),
        id: input.id,
        kind: input.kind,
        label: input.label,
        agent_request: None,
    };
    validate_manual_resource(&value)?;
    Ok(value)
}

fn argument(envelope: &InvocationEnvelope) -> Result<&str, String> {
    let encoded = envelope
        .argument
        .as_deref()
        .ok_or("task arguments missing")?;
    if encoded.len() > MAX_ARGUMENT_BYTES {
        return Err("task arguments exceed bounded input".into());
    }
    Ok(encoded
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(encoded))
}

fn queue_command(envelope: &InvocationEnvelope) -> Result<WorkspaceTaskCommand, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        title: String,
        assigned_agent: String,
        planning: String,
        #[serde(default)]
        resource_json: Option<String>,
    }
    let input: Input =
        serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
    let route = match input.planning.as_str() {
        "model" => WorkspaceTaskRoute::Model,
        "rules_only" => WorkspaceTaskRoute::RulesOnly,
        _ => return Err("managed task planning must be model or rules_only".into()),
    };
    Ok(WorkspaceTaskCommand::Queue {
        title: input.title,
        assigned_agent: input.assigned_agent,
        route,
        resource: input
            .resource_json
            .as_deref()
            .map(resource)
            .transpose()?
            .map(Box::new),
    })
}

pub(super) fn command(
    envelope: &InvocationEnvelope,
) -> Result<Option<WorkspaceTaskCommand>, String> {
    if let Some(id) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "approve-run"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            agent_id: String,
            expected_run_epoch: u64,
            after_revision: u64,
        }
        let input: Input =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        return Ok(Some(WorkspaceTaskCommand::ApproveRun {
            task_id: format!("task:{id}"),
            agent_id: input.agent_id,
            expected_run_epoch: input.expected_run_epoch,
            after_revision: input.after_revision,
        }));
    }
    if let Some(id) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "set-execution-budget"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            expected_run_epoch: u64,
            #[serde(default)]
            expected_budget: Option<collab_types::task_budget::TaskExecutionBudget>,
            #[serde(default)]
            budget: Option<collab_types::task_budget::TaskExecutionBudget>,
        }
        let input: Input =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        if let Some(budget) = input.budget {
            budget.validate()?;
        }
        if let Some(budget) = input.expected_budget {
            budget.validate()?;
        }
        return Ok(Some(WorkspaceTaskCommand::SetExecutionBudget {
            task_id: format!("task:{id}"),
            expected_run_epoch: input.expected_run_epoch,
            expected_budget: input.expected_budget,
            budget: input.budget,
        }));
    }
    if envelope.resource == WORKSPACE && envelope.action == "queue-agent-task" {
        return queue_command(envelope).map(Some);
    }
    if envelope.resource == WORKSPACE && envelope.action == "create-task" {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            title: String,
            #[serde(default)]
            resource_json: Option<String>,
        }
        let input: Input =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        Ok(Some(WorkspaceTaskCommand::CreateManual {
            title: input.title,
            resource: input
                .resource_json
                .as_deref()
                .map(resource)
                .transpose()?
                .map(Box::new),
        }))
    } else if let Some(id) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "attach-resource"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            resource_json: String,
            #[serde(default)]
            expected_resource_id: Option<String>,
        }
        let input: Input =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        if input
            .expected_resource_id
            .as_ref()
            .is_some_and(|value| value.len() > 2048 || value.chars().any(char::is_control))
        {
            return Err("invalid observed attachment id".into());
        }
        Ok(Some(WorkspaceTaskCommand::AttachResource {
            task_id: format!("task:{id}"),
            expected_resource_id: input.expected_resource_id,
            resource: Box::new(resource(&input.resource_json)?),
        }))
    } else {
        Ok(None)
    }
}

fn manual(task: &WorkspaceTask) -> bool {
    task.route == WorkspaceTaskRoute::Manual && task.assigned_agent.is_none()
}

fn queue_action(workspace_label: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    let mut queue = action(
        WORKSPACE,
        "queue-agent-task",
        workspace_label,
        "Queue agent task",
        vec![
            ActionParam::required(
                "title",
                ParamType::Text,
                "Task title; at most 240 characters",
            ),
            ActionParam::required(
                "assigned_agent",
                ParamType::Text,
                "Exact current agent identity in this workspace",
            ),
            ActionParam::required(
                "planning",
                ParamType::Text,
                "model or rules_only; never manual",
            ),
            ActionParam::optional(
                "resource_json",
                ParamType::Text,
                "Ordinary resource JSON: app_id, app_label, id, kind, label; grants no authority",
            ),
        ],
        false,
    );
    queue.description = Some("An admitted person queues work for a current workspace agent. This does not launch the task, mint an execution lease or grant resource authority.".into());
    queue
}

pub(super) fn actions(
    snapshot: &WorkspaceTaskSnapshot,
    workspace_label: &str,
) -> Vec<PublishedAction> {
    use tilo_actions::{ActionParam, ParamType};
    let mut result = vec![action(
        WORKSPACE,
        "create-task",
        workspace_label,
        "Create manual task",
        vec![
            ActionParam::required(
                "title",
                ParamType::Text,
                "Manual coordination title; at most 240 characters",
            ),
            ActionParam::optional(
                "resource_json",
                ParamType::Text,
                "Ordinary resource JSON: app_id, app_label, id, kind, label; grants no authority",
            ),
        ],
        false,
    )];
    result.push(queue_action(workspace_label));
    for task in &snapshot.tasks {
        let target = format!("desktop:{}", task.id);
        result.push(action(
            &target,
            "inspect",
            &task.title,
            "Inspect task execution policy",
            Vec::new(),
            true,
        ));
        if !task.status.is_finished() {
            result.extend(task_grant_actions(&target, &task.title));
        }
        if task.status == tilo_desktop_model::WorkspaceTaskStatus::Queued
            && !manual(task)
            && task.assigned_agent.is_some()
            && task
                .resource
                .as_ref()
                .is_none_or(|resource| resource.agent_request.is_none())
            && snapshot
                .controls
                .get(&task.id)
                .is_some_and(|value| value.active_run.is_none())
        {
            result.push(approve_action(&target, &task.title));
        }
        if !manual(task)
            && !task.status.is_finished()
            && snapshot
                .controls
                .get(&task.id)
                .is_some_and(|control| control.active_run.is_none())
        {
            let mut operation = action(
                &target,
                "set-execution-budget",
                &task.title,
                "Configure bounded task execution",
                vec![
                    ActionParam::required(
                        "expected_run_epoch",
                        ParamType::Integer,
                        "Exact epoch observed through inspect",
                    ),
                    ActionParam::optional(
                        "expected_budget",
                        ParamType::Object,
                        "Exact observed budget, or null when absent",
                    ),
                    ActionParam::optional(
                        "budget",
                        ParamType::Object,
                        "Owner policy: turn_ms up to 900000, total_ms up to7200000, turn <= total; null restores defaults",
                    ),
                ],
                false,
            );
            operation.description = Some("Only the current workspace owner can configure the next task run. This grants no action, model, check or resource authority.".into());
            result.push(operation);
        }
        if manual(task)
            && !task.status.is_finished()
            && snapshot
                .controls
                .get(&task.id)
                .is_some_and(|control| control.active_run.is_none() && control.run_epoch == 0)
        {
            result.push(action(&target, "attach-resource", &task.title, "Attach resource", vec![
                ActionParam::required("resource_json", ParamType::Text, "Ordinary resource JSON: app_id, app_label, id, kind, label; grants no authority"),
                ActionParam::optional("expected_resource_id", ParamType::Text, "Exact current attachment id observed through inspect; omit only when no attachment exists"),
            ], false));
        }
    }
    result
}

/// The actions that edit an open task's grant set.
fn task_grant_actions(target: &str, title: &str) -> [PublishedAction; 2] {
    use tilo_actions::{ActionParam, ParamType};
    let mut grant = action(
        target,
        "grant",
        title,
        "Allow in this task",
        vec![
            ActionParam::required(
                "node",
                ParamType::Text,
                "Where, as a path below the room, e.g. app:so.tilo.mail",
            ),
            ActionParam::required(
                "kinds",
                ParamType::Array,
                "The kinds of action it covers: read, undoable, outside",
            ),
            ActionParam::required("answer", ParamType::Text, "allow, ask or deny"),
        ],
        false,
    );
    grant.description = Some(
        "Work done in this task gets this answer there while the task is open. A task's grants only bound its work: they give nobody more than they hold.".into(),
    );
    let mut revoke = action(
        target,
        "revoke",
        title,
        "Remove from this task's grants",
        vec![ActionParam::required(
            "grant",
            ParamType::Text,
            "The grant's id, as inspect lists it",
        )],
        false,
    );
    revoke.description = Some("Take one grant out of this task's grant set.".into());
    [grant, revoke]
}

fn approve_action(target: &str, title: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    action(
        target,
        "approve-run",
        title,
        "Approve this task run",
        vec![
            ActionParam::required("agent_id", ParamType::Text, "Exact assigned agent"),
            ActionParam::required(
                "expected_run_epoch",
                ParamType::Integer,
                "Observed run epoch",
            ),
            ActionParam::required(
                "after_revision",
                ParamType::Integer,
                "Observed instruction frontier",
            ),
        ],
        false,
    )
}

fn action(
    target: &str,
    operation: &str,
    label: &str,
    operation_label: &str,
    params: Vec<tilo_actions::ActionParam>,
    inspect: bool,
) -> PublishedAction {
    PublishedAction {
        semantic_identity: None,
        resource: target.into(), action: operation.into(), resource_kind: if target == WORKSPACE { "workspace-membership" } else { "workspace-task" }.into(), resource_label: label.into(), action_label: operation_label.into(),
        description: Some(if inspect { "Inspect current task coordination and execution policy; no execution result is asserted." } else if operation == "approve-run" { "Approve the observed queued task for its assigned agent. Existing workspace and action permissions still apply; approval does not assert execution or completion." } else { "Manual coordination only. Attachments are shared resource references; no agent run, execution, integration or verification is asserted." }.into()),
        operation_kind: if inspect { tilo_actions::OperationKind::View } else { tilo_actions::OperationKind::Document }, params,
        contract: tilo_actions::ActionContract {
            kind: if inspect { tilo_actions::ActionKind::Read } else { tilo_actions::ActionKind::Undoable },
            result: tilo_actions::ResultSpec::Json { schema: serde_json::json!({"type":"object"}) },
            ..Default::default()
        }, view: None, input: PublishedInput::None,
    }
}

pub(super) fn is_inspect(envelope: &InvocationEnvelope) -> bool {
    envelope.resource.starts_with(TASK_PREFIX) && envelope.action == "inspect"
}

pub(super) fn inspect(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
) -> Result<serde_json::Value, String> {
    if envelope.argument.is_some() {
        return Err("task inspection takes no arguments".into());
    }
    let task_id = envelope
        .resource
        .strip_prefix("desktop:")
        .ok_or("invalid task resource")?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("task is absent")?;
    let control = snapshot
        .controls
        .get(task_id)
        .ok_or("task control is absent")?;
    let subject = tilo_action_policy::Subject::Task(task.id.clone());
    let held: Vec<tilo_action_policy::Grant> = snapshot
        .grants
        .iter()
        .filter(|grant| grant.subject == subject)
        .cloned()
        .collect();
    let grants: Vec<serde_json::Value> = held
        .iter()
        .map(|grant| {
            serde_json::json!({
                "id": grant.id,
                "node": grant.node,
                "kinds": grant.kinds,
                "answer": grant.answer,
            })
        })
        .collect();
    let mut output = serde_json::json!({"task":task,"grants":grants,
        "grants_read":tilo_action_policy::describe(&held, &tilo_action_policy::Ids),"coordination_only":manual(task),
        "managed_route":!manual(task), "run_epoch":control.run_epoch,
        "execution_budget":control.execution_budget,
        "active_run":control.active_run.is_some(),
        "instruction_revision":control.directions.last().map_or(0, |direction| direction.revision),
        "default_turn_ms":30_000,"default_total_ms":300_000});
    if manual(task) {
        output
            .as_object_mut()
            .ok_or("invalid task inspection object")?
            .insert("managed_execution".into(), serde_json::json!(false));
    }
    Ok(output)
}

pub(super) fn result(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
    device: [u8; 32],
    generation: u64,
) -> Result<serde_json::Value, String> {
    let result = snapshot
        .controls
        .values()
        .flat_map(|control| &control.receipts)
        .find(|receipt| receipt.device == device && receipt.invocation == envelope.invocation.0)
        .ok_or("owner did not retain the exact task command receipt")?;
    let (task_id, budget_change) = match &result.result {
        WorkspaceTaskMutationResult::Queued { task_id }
        | WorkspaceTaskMutationResult::ResourceAttached { task_id }
        | WorkspaceTaskMutationResult::RunStarted { task_id, .. } => (task_id, false),
        WorkspaceTaskMutationResult::ExecutionBudgetSet { task_id } => (task_id, true),
        _ => return Err("task command returned an unexpected managed execution result".into()),
    };
    let task = snapshot
        .tasks
        .iter()
        .find(|task| &task.id == task_id)
        .ok_or("owner task receipt has no canonical task")?;
    let queued_agent = envelope.resource == WORKSPACE && envelope.action == "queue-agent-task";
    let approved = envelope.action == "approve-run";
    if (!approved && !budget_change && !queued_agent && !manual(task))
        || (queued_agent
            && (manual(task)
                || !matches!(result.result, WorkspaceTaskMutationResult::Queued { .. })))
    {
        return Err("task command receipt disagrees with its declared coordination route".into());
    }
    if approved
        && (envelope.resource != format!("desktop:{task_id}")
            || !matches!(
                result.result,
                WorkspaceTaskMutationResult::RunStarted { .. }
            ))
    {
        return Err("approval receipt has another task or command".into());
    }
    Ok(
        serde_json::json!({"generation":generation,"result":result.result,
        "resource":format!("desktop:{task_id}"),"task":task,
        "coordination_only":manual(task),"managed_execution":false,
        "managed_route":!manual(task),"run_approved":approved,"execution_started":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(arguments: &serde_json::Value) -> InvocationEnvelope {
        InvocationEnvelope {
            id: 1,
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(9)),
            resource: WORKSPACE.into(),
            action: "create-task".into(),
            argument: Some(format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                arguments
            )),
            actor: "supplied-actor-is-not-authority".into(),
            context: None,
        }
    }

    fn enrolled_queue_owner() -> (
        tempfile::TempDir,
        tilo_joined_room::NativeWorkspaceOwner,
        tilo_user_identity::DeviceCert,
        ed25519_dalek::SigningKey,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let owner = tilo_joined_room::open_file(&directory.path().join("native.json")).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[92; 32]);
        let certificate = tilo_joined_room::testing::certificate(
            &ed25519_dalek::SigningKey::from_bytes(&[91; 32]),
            &key,
            "Governor",
        );
        let _governor =
            tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate)
                .unwrap();
        (directory, owner, certificate, key)
    }

    fn assert_non_person_queue_refused(
        owner: &tilo_joined_room::NativeWorkspaceOwner,
        request: &tilo_peer_state::owner_commit::OwnerCommitRequest,
        certificate: &tilo_user_identity::DeviceCert,
        snapshot: &WorkspaceTaskSnapshot,
        context: (tilo_desktop_model::RoomId, std::time::SystemTime),
    ) {
        use tilo_peer_state::owner_commit::CommitOperation;
        use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;
        let (room, now) = context;
        for authority in [
            WorkspaceTaskAuthority::Agent {
                attestor: certificate.clone(),
                agent_id: "scribe".into(),
            },
            WorkspaceTaskAuthority::Host {
                attestor: certificate.clone(),
            },
        ] {
            let mut impersonated = request.clone();
            let CommitOperation::WorkspaceTaskCommand {
                invocation,
                authority: current,
                ..
            } = &mut impersonated.operation
            else {
                panic!("expected task command");
            };
            *invocation = uuid::Uuid::new_v4();
            *current = authority;
            assert!(
                owner
                    .commit_workspace_task_command(impersonated, now)
                    .is_err()
            );
            assert_eq!(owner.workspace_task_snapshot(room).unwrap(), *snapshot);
        }
    }

    #[test]
    fn public_queue_commits_an_admitted_agent_and_retains_a_receipt_without_a_run() {
        use tilo_action_policy::roles::AgentPreset;
        use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
        use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;
        let (_directory, owner, certificate, key) = enrolled_queue_owner();
        let now = std::time::SystemTime::now();
        let room = tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        let membership = |command| OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: certificate.clone(),
                command,
            },
        };
        let enrolled = membership(WorkspaceMembershipCommand::AddAgent {
            agent: "scribe".into(),
            preset: AgentPreset::Contributor,
        });
        owner
            .commit_workspace_membership_command(enrolled.clone(), &certificate, &key, now)
            .unwrap()
            .validate_for(&enrolled)
            .unwrap();
        let before = owner.workspace_task_snapshot(room).unwrap();
        let published = actions(&before, "Workspace")
            .into_iter()
            .find(|row| row.action == "queue-agent-task")
            .unwrap();
        assert_eq!(published.contract.kind, tilo_actions::ActionKind::Undoable);
        let mut public = envelope(
            &serde_json::json!({"title":"Inspect chip proof", "assigned_agent":"scribe", "planning":"model"}),
        );
        public.action = published.action;
        public.resource = published.resource;
        let request = OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: public.invocation.0,
                authority: WorkspaceTaskAuthority::Person {
                    caller: certificate.clone(),
                },
                command: command(&public).unwrap().unwrap(),
            },
        };
        let receipt = owner
            .commit_workspace_task_command(request.clone(), now)
            .unwrap();
        receipt.validate_for(&request).unwrap();
        let snapshot = owner.workspace_task_snapshot(room).unwrap();
        let output = result(
            &snapshot,
            &public,
            certificate.device_public_key,
            receipt.generation,
        )
        .unwrap();
        assert_eq!(output["task"]["assigned_agent"], "scribe");
        assert_eq!(output["task"]["status"], "queued");
        assert_eq!(output["managed_route"], true);
        assert_eq!(output["managed_execution"], false);
        assert_eq!(output["execution_started"], false);
        assert!(
            snapshot
                .controls
                .values()
                .all(|control| control.active_run.is_none() && control.run_epoch == 0)
        );
        owner
            .commit_workspace_task_command(request.clone(), now)
            .unwrap()
            .validate_for(&request)
            .unwrap();
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), snapshot);
        assert_non_person_queue_refused(&owner, &request, &certificate, &snapshot, (room, now));
        let removed = membership(WorkspaceMembershipCommand::RemoveAgent {
            agent: "scribe".into(),
        });
        owner
            .commit_workspace_membership_command(removed.clone(), &certificate, &key, now)
            .unwrap()
            .validate_for(&removed)
            .unwrap();
        let mut stale = request;
        let CommitOperation::WorkspaceTaskCommand { invocation, .. } = &mut stale.operation else {
            panic!("expected task command");
        };
        *invocation = uuid::Uuid::new_v4();
        assert!(owner.commit_workspace_task_command(stale, now).is_err());
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), snapshot);
    }

    #[test]
    fn managed_queue_arguments_cannot_choose_authority_or_run_state() {
        for field in [
            "actor",
            "caller",
            "certificate",
            "room",
            "authority",
            "status",
            "run_epoch",
            "lease",
        ] {
            let mut input = serde_json::json!({"title":"Review RTL", "assigned_agent":"scribe", "planning":"model"});
            input[field] = serde_json::json!("forged");
            let mut public = envelope(&input);
            public.action = "queue-agent-task".into();
            assert!(command(&public).is_err(), "{field}");
        }
        for planning in ["manual", "Model", "", "host"] {
            let mut public = envelope(
                &serde_json::json!({"title":"Review RTL", "assigned_agent":"scribe", "planning":planning}),
            );
            public.action = "queue-agent-task".into();
            assert!(command(&public).is_err());
        }
    }

    #[test]
    fn budget_public_arguments_cannot_select_authority_or_unbounded_deadlines() {
        let mut request = envelope(&serde_json::json!({"expected_run_epoch":0,
            "budget":{"turn_ms":900_000,"total_ms":7_200_000}}));
        request.resource = "desktop:task:one".into();
        request.action = "set-execution-budget".into();
        assert!(
            matches!(command(&request).unwrap(), Some(WorkspaceTaskCommand::SetExecutionBudget { task_id, .. }) if task_id == "task:one")
        );
        for input in [
            serde_json::json!({"expected_run_epoch":0,"actor":"owner"}),
            serde_json::json!({"expected_run_epoch":0,"budget":{"turn_ms":900_001,"total_ms":7_200_000}}),
            serde_json::json!({"expected_run_epoch":false}),
            serde_json::json!({"expected_run_epoch":0,"budget":{"turn_ms":100,"total_ms":99}}),
        ] {
            request.argument = Some(format!("{}{}", tilo_actions::ARGUMENTS_JSON_PREFIX, input));
            assert!(command(&request).is_err());
        }
    }

    #[test]
    fn manual_public_contract_refuses_execution_authority_and_instruction_carriers() {
        assert!(matches!(
            command(&envelope(&serde_json::json!({"title":"Review RTL"}))).unwrap(),
            Some(WorkspaceTaskCommand::CreateManual { resource: None, .. })
        ));
        for field in [
            "actor",
            "caller",
            "certificate",
            "room",
            "route",
            "assigned_agent",
            "status",
            "run_epoch",
        ] {
            let mut input = serde_json::json!({"title":"Review RTL"});
            input[field] = serde_json::json!("forged");
            assert!(command(&envelope(&input)).is_err(), "{field}");
        }
        let mut reference = serde_json::json!({"app_id":"so.tilo.chip-design","app_label":"Chip Design","id":"chip_design.verification:hash","kind":"verification","label":"Report"});
        reference["agent_request"] = serde_json::json!({"instruction":"run something"});
        assert!(
            command(&envelope(
                &serde_json::json!({"title":"Review RTL", "resource_json":reference.to_string()})
            ))
            .is_err()
        );
        reference.as_object_mut().unwrap().remove("agent_request");
        for id in ["file:///device/private", "FILE:///device/private"] {
            reference["id"] = serde_json::json!(id);
            assert!(
                command(&envelope(&serde_json::json!({
                    "title":"Review RTL", "resource_json":reference.to_string()
                })))
                .is_err()
            );
        }
        assert!(command(&envelope(&serde_json::json!({"title":"x".repeat(8192)}))).is_err());
    }

    #[test]
    fn task_catalog_exposes_managed_policy_without_execution_authority() {
        let mut document = automerge::AutoCommit::new();
        // Project the real typed production document shape, then read through
        // the same canonical snapshot function used by the installed service.
        let mut task = WorkspaceTask {
            id: "task:00000000-0000-4000-8000-000000000001".into(),
            title: "Review report".into(),
            created_by: "person".into(),
            assigned_agent: None,
            route: WorkspaceTaskRoute::Manual,
            status: tilo_desktop_model::WorkspaceTaskStatus::Queued,
            resource: None,
            created_at: 1,
            updated_at: 1,
            audit: Vec::new(),
        };
        tilo_desktop_model::projection::project_workspace_tasks(&mut document, &[task.clone()])
            .unwrap();
        let mut snapshot = tilo_peer_state::workspace_task::snapshot(&document).unwrap();
        snapshot.controls.insert(
            task.id.clone(),
            tilo_desktop_model::WorkspaceTaskControl::default(),
        );
        let catalogue = actions(&snapshot, "Workspace");
        // An open task offers its grant set's grant and revoke beside its
        // own actions.
        assert_eq!(catalogue.len(), 6);
        assert!(
            ["grant", "revoke"]
                .iter()
                .all(|id| catalogue.iter().any(|row| row.action == *id))
        );
        assert_eq!(catalogue[0].resource_label, "Workspace");
        assert_eq!(catalogue[0].resource_kind, "workspace-membership");
        let inspect_action = catalogue
            .iter()
            .find(|row| row.action == "inspect")
            .unwrap();
        assert_eq!(inspect_action.contract.kind, tilo_actions::ActionKind::Read);
        let mut request = envelope(&serde_json::json!({}));
        request.resource = format!("desktop:{}", task.id);
        request.action = "inspect".into();
        request.argument = None;
        assert_eq!(inspect(&snapshot, &request).unwrap()["task"]["id"], task.id);
        task.assigned_agent = Some("scribe".into());
        task.route = WorkspaceTaskRoute::Model;
        snapshot.tasks = vec![task];
        let managed = actions(&snapshot, "Workspace");
        assert_eq!(managed.len(), 7);
        assert!(managed.iter().any(|row| row.action == "approve-run"));
        assert!(
            managed
                .iter()
                .any(|row| row.action == "set-execution-budget")
        );
        assert!(!managed.iter().any(|row| row.action == "attach-resource"));
        let observed = inspect(&snapshot, &request).unwrap();
        assert_eq!(observed["managed_route"], true);
        assert!(observed.get("managed_execution").is_none());
        assert_eq!(observed["active_run"], false);
        assert_eq!(observed["run_epoch"], 0);
        assert_eq!(observed["execution_budget"], serde_json::Value::Null);
        assert!(observed.get("active_run_token").is_none());
        assert!(observed.get("instruction").is_none());
    }
}
