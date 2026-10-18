//! Exact governor custody over the existing authenticated private connection.
use super::NativeOwnerCommitService;
use tilo_joined_room::task_admission::{NativeAdmissionCommand as C, NativeAdmissionReply as R};
use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;
use tilo_relay_session::rooms::private::{PrivateOperation, PrivateReply};

/// Drive an owned owner operation away from the broker's LocalSet. The runtime
/// still drives remote I/O and timers; synchronous custody RPCs, owner reads and
/// durable writes run on the blocking worker, including after an await.
pub(super) async fn on_owner_worker<T, F, Fut>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || runtime.block_on(work()))
        .await
        .map_err(|_| "native owner operation interrupted; outcome unknown".to_owned())?
}

impl NativeOwnerCommitService {
    pub(super) fn native_task_custody(
        &self,
        room: tilo_desktop_model::RoomId,
        command: &C,
    ) -> Result<R, String> {
        let remote = super::task_mailboxes::remote::route(self, &room.to_string(), None, None)?
            .ok_or("native governor private route unavailable")?;
        let payload = tilo_joined_room::task_admission::encode_admission_command(command)?;
        let PrivateReply::NativeTask { payload } = super::task_mailboxes::remote::command(
            remote,
            PrivateOperation::NativeTask { payload },
        )?
        else {
            return Err("unexpected native custody reply".into());
        };
        tilo_joined_room::task_admission::decode_admission_reply(&payload)
    }
    // Commit and membership execution drive this entire future on an owner
    // worker: both the private RPC and the admission reads below can block.
    pub(super) async fn forward_task_command(
        &self,
        request: &OwnerCommitRequest,
    ) -> Result<OwnerCommitReceipt, String> {
        let CommitOperation::WorkspaceTaskCommand {
            invocation,
            authority,
            command,
        } = &request.operation
        else {
            return Err("not a task command".into());
        };
        let id =
            tilo_desktop_model::RoomId::parse(&request.room).ok_or("invalid native task room")?;
        let owner = &self.opened_owner("native owner unavailable")?;
        owner.with_current(id, |view| {
            if view.device() != authority.certificate() {
                return Err("task authority differs from actual transport device".into());
            }
            Ok(())
        })?;
        let model_start = matches!(authority, WorkspaceTaskAuthority::Person { .. })
            && matches!(
                command,
                tilo_peer_state::workspace_task::WorkspaceTaskCommand::ApproveRun { .. }
            )
            && owner.with_current(id, |view| {
                let tilo_peer_state::workspace_task::WorkspaceTaskCommand::ApproveRun {
                    task_id,
                    ..
                } = command
                else {
                    return Ok(false);
                };
                let state = tilo_peer_state::workspace_task::snapshot(
                    view.peer()
                        .admitted_room()
                        .ok_or("native task room absent")?,
                )?;
                Ok(state.tasks.iter().any(|task| {
                    task.id == *task_id
                        && task.route == tilo_desktop_model::WorkspaceTaskRoute::Model
                }))
            })?;
        let command = match authority {
            WorkspaceTaskAuthority::Person { .. } if model_start => C::PersonModelStart {
                invocation: *invocation,
                command: command.clone(),
                snapshot: self.capture_person_model_start(owner, request)?,
            },
            WorkspaceTaskAuthority::Person { .. } => C::PrivateCoordination {
                invocation: *invocation,
                command: command.clone(),
            },
            WorkspaceTaskAuthority::Agent { agent_id, .. } => C::Report {
                invocation: *invocation,
                agent: Some(agent_id.clone()),
                command: command.clone(),
            },
            WorkspaceTaskAuthority::Host { .. } => C::Report {
                invocation: *invocation,
                agent: None,
                command: command.clone(),
            },
        };
        let R::Coordination(receipt) = self.native_task_custody(id, &command)? else {
            return Err("unexpected task coordination reply".into());
        };
        if receipt.binding != request.binding()? {
            return Err("native task receipt binding mismatch".into());
        }
        // Fence locally immediately for a locally requested Stop. Other hosts
        // receive the same barrier through the native-delivery refresh.
        if owner.with_current(id, |view| {
            Ok(view.device().capabilities.can_act_as_host || view.device() == view.governor())
        })? {
            self.receive_task_fences(id)?;
        }
        let expected = request.binding()?;
        let device = authority.certificate().device_public_key;
        let network = self
            .network
            .as_ref()
            .ok_or("native task network unavailable")?;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                network.check_native_custody(owner, id)?;
                let admitted = owner.with_current(id, |view| {
                    let state = tilo_peer_state::workspace_task::snapshot(
                        view.peer()
                            .admitted_room()
                            .ok_or("native task room unavailable")?,
                    )?;
                    let found = state
                        .controls
                        .values()
                        .flat_map(|control| &control.receipts)
                        .find(|receipt| {
                            receipt.device == device && receipt.invocation == *invocation
                        });
                    let Some(found) = found else {
                        return Ok(None);
                    };
                    if found.request_digest != expected.request_digest {
                        return Err("native task admitted receipt mismatch".into());
                    }
                    Ok(Some(OwnerCommitReceipt {
                        binding: expected.clone(),
                        generation: view.generation(),
                        aggregate_hash: *blake3::hash(&view.peer().durable()?).as_bytes(),
                    }))
                })?;
                if let Some(receipt) = admitted {
                    return Ok(receipt);
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| "signed task receipt did not reach the native owner; outcome unknown")?
    }
    pub(super) fn receive_task_fences(
        &self,
        room: tilo_desktop_model::RoomId,
    ) -> Result<(), String> {
        let R::HostFences(fences) = self.native_task_custody(room, &C::PendingHostFences)? else {
            return Err("unexpected pending fence reply".into());
        };
        let owner = &self.opened_owner("native host unavailable")?;
        let (key, host) = tilo_identity_host::broker::trusted_current_device(&room.to_string())
            .map_err(|_| "native host key unavailable")?;
        for fence in fences {
            let governor = owner.with_current(room, |view| {
                if view.device() != &host {
                    return Err("native fence host changed".into());
                }
                Ok(view.governor().clone())
            })?;
            let ack = owner.acknowledge_native_host_fence(&fence, &governor, &key)?;
            if !matches!(
                self.native_task_custody(room, &C::FenceAcknowledgement(ack))?,
                R::Acknowledged
            ) {
                return Err("unexpected native fence acknowledgement".into());
            }
        }
        Ok(())
    }
}
