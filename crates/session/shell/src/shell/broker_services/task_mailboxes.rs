//! Private mail is committed by the native owner, not by a document operation.
//!
//! Remote native reads bind the current source task/run to the authenticated
//! physical host. Inbox writes also carry the registered action's exact
//! governor dispatch custody; a caller-supplied sender cannot select a source.
#[path = "task_private_remote.rs"]
pub(super) mod remote;
use super::NativeOwnerCommitService;
use tilo_action_broker::services::{NativeTaskActionRequirement, NativeTaskBinding};
use tilo_actions::task_messaging::{TaskMessage, TaskMessageSend, TaskRunAddress};
use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;
use tilo_relay_session::rooms::private::{PrivateOperation, PrivateReply};

fn native_operation(
    binding: &NativeTaskBinding,
    operation: PrivateOperation,
    requirement: Option<&NativeTaskActionRequirement>,
) -> Result<PrivateOperation, String> {
    Ok(PrivateOperation::NativeTaskRead {
        task: binding.task_id.clone(),
        epoch: binding.epoch,
        agent: binding.agent_id.clone(),
        evidence: requirement
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| "native task contract encoding failed")?
            .unwrap_or_default(),
        operation: Box::new(operation),
    })
}

pub(super) fn owner(
    service: &NativeOwnerCommitService,
    room: &str,
) -> Result<
    (
        tilo_joined_room::NativeWorkspaceOwner,
        tilo_desktop_model::RoomId,
    ),
    String,
> {
    let room = tilo_desktop_model::RoomId::parse(room).ok_or("invalid task mailbox room")?;
    let owner = service.opened_owner("task mailbox owner unavailable")?;
    if let Some(network) = &service.network {
        network.check_native_custody(&owner, room)?;
        if network.governed_elsewhere(room) {
            return Err(
                "private task access requires authenticated forwarding to its governor".into(),
            );
        }
    }
    // A joined replica carries public controls, not the governor's private store.
    // Never interpret an empty replica mailbox as a successful remote read.
    owner.with_current(room, |view| {
        if view.device() != view.governor() {
            return Err("private task access requires the actual governing owner".into());
        }
        Ok(())
    })?;
    Ok((owner.clone(), room))
}

pub(super) fn messages(
    service: &NativeOwnerCommitService,
    room: &str,
    binding: &NativeTaskBinding,
) -> Result<Vec<TaskMessage>, String> {
    if let Some(route) = remote::route(service, room, Some(binding), None)? {
        let PrivateReply::TaskMessages(messages) = remote::command(
            route,
            native_operation(
                binding,
                PrivateOperation::TaskMessages {
                    task: binding.task_id.clone(),
                    epoch: binding.epoch,
                },
                None,
            )?,
        )?
        else {
            return Err("unexpected private task messages reply".into());
        };
        return Ok(messages);
    }
    let (owner, room) = owner(service, room)?;
    let authority = agent_authority(&owner, room, binding)?;
    owner.task_messages(room, &binding.task_id, binding.epoch, &authority, true)
}

pub(super) fn agent_authority(
    owner: &tilo_joined_room::NativeWorkspaceOwner,
    room: tilo_desktop_model::RoomId,
    binding: &NativeTaskBinding,
) -> Result<WorkspaceTaskAuthority, String> {
    let attestor = owner.with_current(room, |view| {
        // Only the broker's admitted NativeTaskExecution channel supplies this
        // binding. A host pin alone does not give this process that identity.
        let local = view.device();
        let pinned = view.host().unwrap_or_else(|| view.governor());
        if local != pinned {
            return Err("private task reader lacks local execution host custody".into());
        }
        check_run(view.peer(), binding, false)?;
        Ok(local.clone())
    })?;
    Ok(WorkspaceTaskAuthority::Agent {
        attestor,
        agent_id: binding.agent_id.clone(),
    })
}

fn check_run(
    peer: &tilo_peer_state::StatePeer,
    binding: &NativeTaskBinding,
    check_revision: bool,
) -> Result<(), String> {
    let snapshot = tilo_peer_state::workspace_task::snapshot(
        peer.admitted_room()
            .ok_or("private task room unavailable")?,
    )?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == binding.task_id)
        .ok_or("private task absent")?;
    let control = snapshot
        .controls
        .get(&binding.task_id)
        .ok_or("private task control absent")?;
    let run = control
        .active_run
        .as_ref()
        .ok_or("private task run ended")?;
    if task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
        || task.assigned_agent.as_deref() != Some(binding.agent_id.as_str())
        || run.agent_id != binding.agent_id
        || run.epoch != binding.epoch
        || control.run_epoch != binding.epoch
        || (check_revision
            && (run.applied_direction_revision != binding.revision
                || control
                    .directions
                    .last()
                    .map_or(0, |direction| direction.revision)
                    != binding.revision))
    {
        return Err("private task binding stopped, replaced or stale".into());
    }
    let approval = run
        .approval
        .as_ref()
        .ok_or("private task run approval absent")?;
    if binding.starter.as_deref() != Some(approval.person.as_str()) {
        return Err("private task starter binding changed".into());
    }
    peer.workspace().check_task_execution_actor(
        &binding.agent_id,
        Some(approval),
        std::time::SystemTime::now(),
    )
}

pub(super) fn acknowledge(
    service: &NativeOwnerCommitService,
    room: &str,
    binding: &NativeTaskBinding,
    ids: &[String],
) -> Result<(), String> {
    if let Some(route) = remote::route(service, room, Some(binding), None)? {
        return match remote::command(
            route,
            native_operation(
                binding,
                PrivateOperation::TaskAcknowledge {
                    task: binding.task_id.clone(),
                    epoch: binding.epoch,
                    ids: ids.to_vec(),
                },
                None,
            )?,
        )? {
            PrivateReply::Done => Ok(()),
            _ => Err("unexpected private task acknowledgement reply".into()),
        };
    }
    let (owner, room) = owner(service, room)?;
    let authority = agent_authority(&owner, room, binding)?;
    owner.acknowledge_task_messages(room, &binding.task_id, binding.epoch, &authority, ids)
}

pub(super) fn person_messages(
    service: &NativeOwnerCommitService,
    room: &str,
    task: &str,
    epoch: u64,
    caller: &tilo_user_identity::DeviceCert,
) -> Result<Vec<TaskMessage>, String> {
    if let Some(route) = remote::route(service, room, None, Some(caller))? {
        let PrivateReply::TaskMessages(messages) = remote::command(
            route,
            PrivateOperation::TaskMessages {
                task: task.into(),
                epoch,
            },
        )?
        else {
            return Err("unexpected private task messages reply".into());
        };
        return Ok(messages);
    }
    let (owner, room) = owner(service, room)?;
    owner.task_messages(
        room,
        task,
        epoch,
        &WorkspaceTaskAuthority::Person {
            caller: caller.clone(),
        },
        false,
    )
}

pub(super) fn send(
    service: &NativeOwnerCommitService,
    room: &str,
    binding: &NativeTaskBinding,
    recipient: &str,
    invocation: uuid::Uuid,
    send: TaskMessageSend,
    requirement: &NativeTaskActionRequirement,
) -> Result<TaskMessage, String> {
    requirement.check_task_message_payload(recipient, invocation, &send)?;
    if requirement.binding.as_ref().is_none_or(|bound| {
        bound.task_id != binding.task_id
            || bound.agent_id != binding.agent_id
            || bound.epoch != binding.epoch
            || bound.revision != binding.revision
            || bound.starter != binding.starter
    }) || requirement.app != "so.tilo.desktop"
        || requirement.resource_kind != "workspace-task"
        || requirement.resource != format!("desktop:{recipient}")
        || requirement.action != "send-message"
        || requirement.need.field.as_deref() != Some("inbox")
    {
        return Err("task mailbox capability binding mismatch".into());
    }
    if requirement
        .invocation
        .as_ref()
        .is_none_or(|exact| exact.id != invocation)
    {
        return Err("task mailbox invocation binding mismatch".into());
    }
    if let Some(route) = remote::route(service, room, Some(binding), None)? {
        let room_id = tilo_desktop_model::RoomId::parse(room).ok_or("invalid private task room")?;
        service
            .opened_owner("private task owner unavailable")?
            .authorize_retained_native_invocation(room_id, invocation, |view, exact, now| {
                check_run(view.peer(), binding, true)?;
                view.check_retained_native_task_action(requirement, exact, now)
            })?;
        return match remote::command(
            route,
            native_operation(
                binding,
                PrivateOperation::TaskSend {
                    recipient: recipient.into(),
                    invocation,
                    send,
                },
                Some(requirement),
            )?,
        )? {
            PrivateReply::TaskSent(message) => Ok(message),
            _ => Err("unexpected private task send reply".into()),
        };
    }
    let (owner, room_id) = owner(service, room)?;
    agent_authority(&owner, room_id, binding)?;
    owner.send_task_message(
        room_id,
        TaskRunAddress {
            task_id: binding.task_id.clone(),
            run_epoch: binding.epoch,
            agent_id: binding.agent_id.clone(),
        },
        recipient,
        invocation,
        send,
        |view| {
            check_run(view.peer(), binding, true)?;
            // Observe within the existing owner turn without nesting owner locks.
            let exact = view.retained_native_invocation(invocation)?;
            view.check_retained_native_task_action(requirement, exact, std::time::SystemTime::now())
        },
    )
}

pub(super) fn record_outputs(
    service: &NativeOwnerCommitService,
    room: &str,
    binding: &NativeTaskBinding,
    outputs: Vec<tilo_actions::task_messaging::TaskOutputRef>,
    requirement: &NativeTaskActionRequirement,
) -> Result<(), String> {
    if requirement.binding.as_ref().is_none_or(|bound| {
        bound.task_id != binding.task_id
            || bound.agent_id != binding.agent_id
            || bound.epoch != binding.epoch
            || bound.revision != binding.revision
            || bound.starter != binding.starter
    }) {
        return Err("task output source capability binding mismatch".into());
    }
    let address = TaskRunAddress {
        task_id: binding.task_id.clone(),
        run_epoch: binding.epoch,
        agent_id: binding.agent_id.clone(),
    };
    let invocation = requirement
        .invocation
        .as_ref()
        .ok_or("task output invocation absent")?
        .id;
    for output in &outputs {
        output.validate()?;
        if output.task_run != address
            || uuid::Uuid::parse_str(&output.invocation).ok() != Some(invocation)
        {
            return Err("task output lacks original source custody".into());
        }
    }
    if let Some(route) = remote::route(service, room, Some(binding), None)? {
        return match remote::command(
            route,
            native_operation(
                binding,
                PrivateOperation::TaskRecordOutputs {
                    task: binding.task_id.clone(),
                    epoch: binding.epoch,
                    outputs,
                },
                Some(requirement),
            )?,
        )? {
            PrivateReply::Done => Ok(()),
            _ => Err("unexpected native output receipt".into()),
        };
    }
    let (owner, room_id) = owner(service, room)?;
    agent_authority(&owner, room_id, binding)?;
    owner.record_task_outputs(room_id, &address, outputs, |view| {
        // A completed invocation may precede a new direction; retain its real
        // output while still rechecking the live run and current authority.
        check_run(view.peer(), binding, false)?;
        let exact = view.retained_native_invocation(invocation)?;
        view.check_retained_native_task_action(requirement, exact, std::time::SystemTime::now())
    })
}

#[cfg(test)]
#[path = "task_mailboxes_tests.rs"]
mod tests;

fn report_dtos(
    reports: Vec<tilo_joined_room::NativePrivateTaskReport>,
) -> Result<Vec<tilo_actions::task_messaging::TaskReport>, String> {
    use tilo_actions::task_messaging::{TaskReport, TaskReportKind as K, TaskReportRole};
    use tilo_joined_room::{NativePrivateTaskReportKind as N, NativePrivateTaskReportRole};
    reports
        .into_iter()
        .map(|report| {
            let report = TaskReport {
                invocation: report.invocation,
                source_device: report.source_device,
                role: match report.role {
                    NativePrivateTaskReportRole::Assistant => TaskReportRole::Assistant,
                },
                accepted_at_unix_ns: report.accepted_at_unix_ns,
                sequence: report.sequence,
                task_run: report.task_run,
                actor: report.actor,
                kind: match report.kind {
                    N::Presentation => K::Presentation,
                    N::Progress => K::Progress,
                    N::Finish => K::Finish,
                    N::Failure => K::Failure,
                },
                body: report.body,
                presentation: report.presentation,
            };
            report.validate()?;
            Ok(report)
        })
        .collect()
}

pub(super) fn person_reports(
    service: &NativeOwnerCommitService,
    room: &str,
    task: &str,
    epoch: u64,
    caller: &tilo_user_identity::DeviceCert,
) -> Result<Vec<tilo_actions::task_messaging::TaskReport>, String> {
    if let Some(route) = remote::route(service, room, None, Some(caller))? {
        return match remote::command(
            route,
            PrivateOperation::TaskReportTranscript {
                task: task.into(),
                epoch,
            },
        )? {
            PrivateReply::TaskReportTranscript(reports) => {
                for report in &reports {
                    report.validate()?;
                }
                Ok(reports)
            }
            _ => Err("unexpected private task transcript reply".into()),
        };
    }
    let (owner, room) = owner(service, room)?;
    owner
        .task_report_transcript(
            room,
            task,
            epoch,
            &WorkspaceTaskAuthority::Person {
                caller: caller.clone(),
            },
        )
        .and_then(report_dtos)
}

pub(super) fn reports(
    service: &NativeOwnerCommitService,
    room: &str,
    binding: &NativeTaskBinding,
) -> Result<Vec<tilo_actions::task_messaging::TaskReport>, String> {
    if let Some(route) = remote::route(service, room, Some(binding), None)? {
        return match remote::command(
            route,
            native_operation(
                binding,
                PrivateOperation::TaskReportTranscript {
                    task: binding.task_id.clone(),
                    epoch: binding.epoch,
                },
                None,
            )?,
        )? {
            PrivateReply::TaskReportTranscript(reports) => {
                for report in &reports {
                    report.validate()?;
                }
                Ok(reports)
            }
            _ => Err("unexpected native task transcript reply".into()),
        };
    }
    let (owner, room) = owner(service, room)?;
    let authority = agent_authority(&owner, room, binding)?;
    owner
        .task_report_transcript(room, &binding.task_id, binding.epoch, &authority)
        .and_then(report_dtos)
}

pub(super) fn person_outputs(
    service: &NativeOwnerCommitService,
    room: &str,
    task: &str,
    epoch: u64,
    caller: &tilo_user_identity::DeviceCert,
) -> Result<Vec<tilo_actions::task_messaging::TaskOutputRef>, String> {
    if let Some(route) = remote::route(service, room, None, Some(caller))? {
        return match remote::command(
            route,
            PrivateOperation::TaskOutputs {
                task: task.into(),
                epoch,
            },
        )? {
            PrivateReply::TaskOutputs(outputs) => Ok(outputs),
            _ => Err("unexpected private task outputs reply".into()),
        };
    }
    let (owner, room) = owner(service, room)?;
    owner.task_outputs(
        room,
        task,
        epoch,
        &WorkspaceTaskAuthority::Person {
            caller: caller.clone(),
        },
    )
}
