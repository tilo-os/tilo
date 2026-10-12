//! Broker-authenticated workspace collaboration administration surface.

use super::room_service::{ServiceOutcome, refused};
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
        let peer_id = self.next_service_peer_id();
        let surface = PublishedSurface {
            peer_id,
            app_id: collaboration::APP_ID.into(),
            route: CrdtRoute::record(collaboration::APP_ID, &room, ""),
            live: true,
            actions: collaboration_actions(&room, service.supports_media_governance()),
        };
        self.serve_room(
            surface,
            move |state, envelope| {
                let service = service.clone();
                async move {
                    match state
                        .execute_workspace_collaboration(peer_id, &envelope, service.as_ref())
                        .await
                    {
                        Ok(value) => ServiceOutcome::Succeeded(value),
                        Err(collaboration::CollaborationAdminError::Rejected(message)) => {
                            refused("workspace_collaboration_denied", message)
                        }
                        Err(collaboration::CollaborationAdminError::Semantic { code, message }) => {
                            refused(code.as_str(), message)
                        }
                        Err(collaboration::CollaborationAdminError::Uncertain(reason)) => {
                            ServiceOutcome::Uncertain(reason)
                        }
                    }
                }
            },
            |_, _| {},
        )
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
            semantic_identity: None,
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
