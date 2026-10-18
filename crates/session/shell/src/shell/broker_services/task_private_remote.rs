//! Private forwarding authenticates the actual device, never a claimed task actor.
use super::{NativeOwnerCommitService, check_run};
use crate::shell::network::PrivateChatRoute;
use std::time::{Duration, SystemTime};
use tilo_action_broker::services::NativeTaskBinding;
use tilo_relay_session::rooms::private::{PrivateOperation, PrivateReply};
use tilo_user_identity::DeviceCert;

const TIMEOUT: Duration = Duration::from_secs(30);
static COMMANDS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(32);
static WATCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(64);

/// One private, exact-run stream; dropping the receiver cancels the outstanding RPC.
/// Closure/refusal fails the broker's observation instead of masquerading as silence.
pub(in crate::shell::broker_services) fn subscribe(
    service: &NativeOwnerCommitService,
    room: &str,
    binding: &NativeTaskBinding,
) -> Result<Option<tokio::sync::watch::Receiver<()>>, String> {
    let Some(route) = route(service, room, Some(binding), None)? else {
        return Ok(None);
    };
    let permit = WATCHES
        .try_acquire()
        .map_err(|_| "private task watch capacity exhausted")?;
    let runtime = route.runtime()?;
    let binding = binding.clone();
    let (sender, receiver) = tokio::sync::watch::channel(());
    runtime.spawn(async move {
        let _permit = permit;
        let mut after_sequence = 0;
        loop {
            let operation = PrivateOperation::NativeTaskRead {
                task: binding.task_id.clone(),
                epoch: binding.epoch,
                agent: binding.agent_id.clone(),
                evidence: Vec::new(),
                operation: Box::new(PrivateOperation::TaskMailboxWatch {
                    task: binding.task_id.clone(),
                    epoch: binding.epoch,
                    after_sequence,
                }),
            };
            let reply = tokio::select! {
                () = sender.closed() => break,
                reply = route.send(operation) => reply,
            };
            let Ok(PrivateReply::Changed(sequence)) = reply else {
                break;
            };
            if sequence <= after_sequence {
                break;
            }
            after_sequence = sequence;
            if sender.send(()).is_err() {
                break;
            }
        }
    });
    Ok(Some(receiver))
}

pub(in crate::shell::broker_services) fn route(
    service: &NativeOwnerCommitService,
    room: &str,
    binding: Option<&NativeTaskBinding>,
    caller: Option<&DeviceCert>,
) -> Result<Option<PrivateChatRoute>, String> {
    let Some(network) = &service.network else {
        return Ok(None);
    };
    let room = tilo_desktop_model::RoomId::parse(room).ok_or("invalid private task room")?;
    let owner = service.opened_owner("private task owner unavailable")?;
    network.check_native_custody(&owner, room)?;
    let actual = owner.with_current(room, |view| {
        let actual = view.device();
        if caller.is_some_and(|caller| caller != actual) {
            return Err("private task caller differs from actual current device".into());
        }
        if view.device() != view.governor() {
            if let Some(binding) = binding {
                if (!actual.capabilities.can_act_as_host && actual != view.governor())
                    || actual.user_public_key != view.governor().user_public_key
                    || !view.peer().certificates().contains(actual)
                {
                    return Err(
                        "private native forwarding requires current physical host custody".into(),
                    );
                }
                view.peer()
                    .permissions(actual.device_public_key, SystemTime::now())?;
                check_run(view.peer(), binding, false)?;
            }
        }
        Ok(actual.clone())
    })?;
    network.private_chat_route(room, &actual)
}

pub(in crate::shell::broker_services) fn command(
    route: PrivateChatRoute,
    operation: PrivateOperation,
) -> Result<PrivateReply, String> {
    let permit = COMMANDS
        .try_acquire()
        .map_err(|_| "private task capacity exhausted")?;
    let runtime = route.runtime()?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let task = runtime.spawn(async move {
        let result = tokio::time::timeout(TIMEOUT, route.send(operation))
            .await
            .map_err(|_| "private task timed out; outcome unknown".to_string())
            .and_then(|result| result);
        let _ = sender.send(result);
    });
    let result = receiver
        .recv_timeout(TIMEOUT + Duration::from_secs(1))
        .map_err(|_| "private task ended; outcome unknown".to_string());
    task.abort();
    drop(permit);
    result?
}

pub(in crate::shell::broker_services) fn person_instructions(
    service: &NativeOwnerCommitService,
    room: &str,
    task: &str,
    epoch: u64,
    caller: &DeviceCert,
) -> Result<Option<tilo_joined_room::NativePrivateTaskInstructions>, String> {
    let Some(route) = route(service, room, None, Some(caller))? else {
        return Ok(None);
    };
    let PrivateReply::TaskInstructions(text) = command(
        route,
        PrivateOperation::TaskInstructions {
            task: task.into(),
            epoch,
        },
    )?
    else {
        return Err("unexpected private task instructions reply".into());
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|_| "invalid private task instructions reply".into())
}

pub(in crate::shell::broker_services) fn instructions(
    service: &NativeOwnerCommitService,
    room: &str,
    binding: &NativeTaskBinding,
) -> Result<Option<tilo_joined_room::NativePrivateTaskInstructions>, String> {
    let Some(route) = route(service, room, Some(binding), None)? else {
        return Ok(None);
    };
    let PrivateReply::TaskInstructions(text) = command(
        route,
        PrivateOperation::NativeTaskRead {
            task: binding.task_id.clone(),
            epoch: binding.epoch,
            agent: binding.agent_id.clone(),
            evidence: Vec::new(),
            operation: Box::new(PrivateOperation::TaskInstructions {
                task: binding.task_id.clone(),
                epoch: binding.epoch,
            }),
        },
    )?
    else {
        return Err("unexpected private task instructions reply".into());
    };
    let text = serde_json::from_str(&text)
        .map_err(|_| "invalid private task instructions reply".to_string())?;
    Ok(Some(text))
}
