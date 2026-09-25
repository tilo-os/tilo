//! Broker-authenticated workspace collaboration administration surface.

use super::{
    Arc, CapnpBrokerState, CrdtRoute, InvocationEnvelope, PublishedAction, PublishedInput,
    PublishedSurface,
};
use crate::collaboration_admin as collaboration;

impl CapnpBrokerState {
    pub(super) fn start_workspace_collaboration(self: &Arc<Self>) -> Result<(), String> {
        let Some(service) = self.services.collaboration_admin.clone() else {
            return Ok(());
        };
        self.owner_health()?;
        let room = service.room().to_string();
        uuid::Uuid::parse_str(&room).map_err(|error| error.to_string())?;
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
                    app_id: collaboration::APP_ID.into(),
                    route: CrdtRoute::record(collaboration::APP_ID, &room, ""),
                    live: true,
                    actions: collaboration_actions(&room, service.supports_media_governance()),
                },
            );
        let state = self.clone();
        tokio::task::spawn_local(async move {
            while let Some(envelope) = receiver.recv().await {
                let outcome = state
                    .execute_workspace_collaboration(peer_id, &envelope, service.as_ref())
                    .await;
                let update = match outcome {
                    Ok(value) => tilo_actions::InvocationUpdate::Succeeded {
                        invocation: envelope.invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(value),
                            resources: vec![],
                        },
                    },
                    Err(collaboration::CollaborationAdminError::Rejected(message)) => {
                        tilo_actions::InvocationUpdate::Failed {
                            invocation: envelope.invocation,
                            error: tilo_actions::ActionFailure {
                                code: "workspace_collaboration_denied".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        }
                    }
                    Err(collaboration::CollaborationAdminError::Semantic { code, message }) => {
                        tilo_actions::InvocationUpdate::Failed {
                            invocation: envelope.invocation,
                            error: tilo_actions::ActionFailure {
                                code: code.as_str().into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        }
                    }
                    Err(collaboration::CollaborationAdminError::Uncertain(reason)) => {
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

    async fn execute_workspace_collaboration(
        &self,
        peer_id: u64,
        envelope: &InvocationEnvelope,
        service: &dyn collaboration::CollaborationAdmin,
    ) -> Result<serde_json::Value, collaboration::CollaborationAdminError> {
        use collaboration::CollaborationAdminError::{Rejected, Uncertain};
        let staged = self
            .pending_invocations
            .lock()
            .map_err(|_| Uncertain("invocation registry unavailable".into()))?
            .get(&envelope.id)
            .filter(|pending| {
                pending.peer_id == peer_id && pending.update.invocation() == envelope.invocation
            })
            .cloned()
            .ok_or_else(|| Rejected("collaboration invocation is no longer admitted".into()))?;
        let evidence = staged.network_evidence.as_ref().ok_or_else(|| {
            Rejected(
                "collaboration administration requires certified durable caller evidence".into(),
            )
        })?;
        let current = self
            .verify_network_action_caller(evidence)
            .map_err(Rejected)?;
        let key = staged
            .journal_key
            .as_ref()
            .ok_or_else(|| Rejected("collaboration invocation has no durable admission".into()))?;
        if key.owner != current.owner()
            || key.room != service.room()
            || key.invocation != envelope.invocation
            || envelope.resource != collaboration::resource(service.room())
        {
            return Err(Rejected("collaboration invocation binding changed".into()));
        }
        let operation =
            collaboration::parse_operation(&envelope.action, envelope.argument.as_deref())
                .map_err(Rejected)?;
        let caller = evidence.certificate().clone();
        let result = service
            .execute(collaboration::CollaborationAdminRequest {
                room: key.room.clone(),
                invocation: envelope.invocation,
                caller: caller.clone(),
                operation,
            })
            .await;
        if let Err(collaboration::CollaborationAdminError::Rejected(reason)) = &result {
            tracing::warn!(
                room = %key.room,
                request_id = %envelope.invocation.0,
                device = %hex::encode(caller.device_public_key),
                action = %envelope.action,
                outcome = "denied",
                %reason,
                "workspace collaboration administration denied"
            );
        }
        result
    }
}

fn collaboration_actions(room: &str, media: bool) -> Vec<PublishedAction> {
    let resource = collaboration::resource(room);
    collaboration::action_definitions()
        .into_iter()
        .chain(
            media
                .then(collaboration::media_action_definitions)
                .into_iter()
                .flatten(),
        )
        .map(|definition| PublishedAction {
            resource: resource.clone(),
            action: definition.id.into(),
            resource_kind: "workspace".into(),
            resource_label: "Workspace collaboration".into(),
            action_label: definition.label.into(),
            description: Some(definition.description.into()),
            operation_kind: tilo_actions::OperationKind::Effect,
            params: definition.params,
            contract: definition.contract,
            view: None,
            input: PublishedInput::None,
        })
        .collect()
}

#[cfg(test)]
mod room_media_surface_tests {
    use super::*;
    #[test]
    fn collaboration_admin_media_catalogue_requires_explicit_native_support() {
        let legacy = collaboration_actions("room", false);
        assert_eq!(legacy.len(), 5);
        assert!(legacy.iter().all(|a| !a.action.starts_with("media.")));
        let native = collaboration_actions("room", true);
        assert_eq!(native.len(), 10);
        assert!(native.iter().all(|a| a.resource == "workspace:room"));
        assert!(
            native
                .iter()
                .any(|a| a.action == collaboration::MEDIA_CREATE_ACTION)
        );
    }
}
