//! Native semantic service consuming the ordinary durable invocation stream.
use super::*;
use qos_peer_state::desktop_command::{self as command, DesktopCommandError};

impl CapnpBrokerState {
    pub(super) fn start_desktop_commands(self: &Arc<Self>) -> Result<(), String> {
        let Some(service) = self.services.desktop_commands.clone() else {
            return Ok(());
        };
        self.owner_health()?;
        let room = service.room().to_string();
        uuid::Uuid::parse_str(&room).map_err(|e| e.to_string())?;
        let peer_id = self.next_peer_id();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.invocation_subscribers
            .lock()
            .map_err(|_| "invocation subscribers unavailable")?
            .insert(peer_id, sender);
        self.stable_invocation_subscribers
            .lock()
            .map_err(|_| "stable invocation registry unavailable")?
            .insert(peer_id);
        self.surfaces
            .lock()
            .map_err(|_| "action surfaces unavailable")?
            .insert(
                peer_id,
                PublishedSurface {
                    peer_id,
                    app_id: command::APP_ID.into(),
                    route: CrdtRoute::record(command::APP_ID, &room, ""),
                    live: true,
                    actions: vec![PublishedAction {
                        resource: command::RESOURCE.into(),
                        action: command::ACTION.into(),
                        resource_kind: "desktop".into(),
                        resource_label: "Desktop".into(),
                        action_label: "Change desktop".into(),
                        description: Some("Typed native current-room desktop command".into()),
                        operation_kind: tilo_actions::OperationKind::Effect,
                        params: vec![
                            tilo_actions::ActionParam::required(
                                "command",
                                tilo_actions::ParamType::Text,
                                "Exact source-bound desktop command",
                            ),
                            tilo_actions::ActionParam::optional(
                                "committed_desktop_v2",
                                tilo_actions::ParamType::Boolean,
                                "Receive the committed desktop immediately; defaults to false",
                            ),
                        ],
                        contract: tilo_actions::ActionContract {
                            permission: tilo_actions::PermissionRequirement::Write,
                            ..Default::default()
                        },
                        input: PublishedInput::None,
                    }],
                },
            );
        let state = self.clone();
        tokio::task::spawn_local(async move {
            while let Some(envelope) = receiver.recv().await {
                let outcome = state
                    .execute_desktop_command(peer_id, &envelope, service.as_ref())
                    .await;
                let update = match outcome {
                    Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                        invocation: envelope.invocation,
                        output: Default::default(),
                    },
                    Err(DesktopCommandError::Rejected(message)) => {
                        tilo_actions::InvocationUpdate::Failed {
                            invocation: envelope.invocation,
                            error: tilo_actions::ActionFailure {
                                code: "desktop_command_rejected".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        }
                    }
                    Err(DesktopCommandError::Uncertain(reason)) => {
                        if let Ok(mut failure) = state.owner_commit_failure.lock() {
                            failure.get_or_insert(reason);
                        }
                        break;
                    }
                };
                if let Err(error) = state
                    .apply_invocation_update(peer_id, envelope.id, update, None)
                    .await
                {
                    if let Ok(mut failure) = state.owner_commit_failure.lock() {
                        failure.get_or_insert(error.to_string());
                    }
                    break;
                }
            }
            if let Ok(mut surfaces) = state.surfaces.lock() {
                surfaces.remove(&peer_id);
            }
        });
        Ok(())
    }

    async fn execute_desktop_command(
        &self,
        peer_id: u64,
        envelope: &InvocationEnvelope,
        service: &dyn crate::services::DesktopCommands,
    ) -> Result<(), DesktopCommandError> {
        use DesktopCommandError::{Rejected, Uncertain};
        let staged = self
            .pending_invocations
            .lock()
            .map_err(|_| Uncertain("invocation registry unavailable".into()))?
            .get(&envelope.id)
            .filter(|pending| {
                pending.peer_id == peer_id && pending.update.invocation() == envelope.invocation
            })
            .cloned()
            .ok_or_else(|| Rejected("desktop invocation is no longer admitted".into()))?;
        // Only the opaque certified evidence saved by ordinary admission is
        // authority. Actor strings or certificates in arguments are never read.
        let evidence = staged.network_evidence.as_ref().ok_or_else(|| {
            Rejected("desktop commands require certified durable caller evidence".into())
        })?;
        let current = self
            .verify_network_action_caller_with_policy_time(evidence, staged.policy_time.as_ref())
            .map_err(Rejected)?;
        let key = staged
            .journal_key
            .as_ref()
            .ok_or_else(|| Rejected("desktop invocation has no durable admission".into()))?;
        if key.owner != current.owner()
            || key.room != service.room()
            || key.invocation != envelope.invocation
            || envelope.resource != command::RESOURCE
            || envelope.action != command::ACTION
        {
            return Err(Rejected("desktop invocation binding changed".into()));
        }
        let argument: serde_json::Value = serde_json::from_str(
            envelope
                .argument
                .as_deref()
                .ok_or_else(|| Rejected("desktop command arguments missing".into()))?,
        )
        .map_err(|e| Rejected(e.to_string()))?;
        let encoded = argument
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Rejected("desktop command must preserve exact serialized IDs".into()))?;
        if encoded.len() > 32 * 1024 {
            return Err(Rejected("desktop command exceeds bounded size".into()));
        }
        let command: command::BoundCommand =
            serde_json::from_str(encoded).map_err(|e| Rejected(e.to_string()))?;
        let request = qos_peer_state::owner_commit::OwnerCommitRequest {
            room: key.room.clone(),
            operation: qos_peer_state::owner_commit::CommitOperation::DesktopCommand {
                invocation: envelope.invocation.0,
                caller: evidence.certificate().clone(),
                command,
            },
        };
        let direct_delivery = argument
            .get("committed_desktop_v2")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        let receipt = service.execute(request.clone(), direct_delivery).await?;
        receipt.validate_for(&request).map_err(Uncertain)?;
        Ok(())
    }
}
