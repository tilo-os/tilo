//! Documents proposed into the room, and the owner's answers, as ordinary
//! published Effect actions: each one leaves a receipt in the room's
//! activity. The caller comes only from certified network admission.
use super::{
    Arc, CapnpBrokerState, CrdtRoute, InvocationEnvelope, PublishedAction, PublishedInput,
    PublishedSurface,
};
use tilo_peer_state::desktop_command::DesktopCommandError;
use tilo_peer_state::document_import::{
    self as import, ACCEPT, DECLINE, ImportAction, PROPOSE, RESOURCE,
};

/// The one parameter every import action takes: its typed argument,
/// serialized as JSON text so no identity loses precision on the way.
pub(crate) const ARGUMENT: &str = "import";

impl CapnpBrokerState {
    pub(super) fn start_document_imports(self: &Arc<Self>) -> Result<(), String> {
        let Some(service) = self.services.document_imports.clone() else {
            return Ok(());
        };
        self.owner_health()?;
        let room = service.room().to_string();
        uuid::Uuid::parse_str(&room).map_err(|e| e.to_string())?;
        let peer_id = self.next_service_peer_id();
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
                    app_id: import::APP_ID.into(),
                    route: CrdtRoute::record(import::APP_ID, &room, ""),
                    live: true,
                    actions: vec![
                        import_action(PROPOSE, "Propose a document for this workspace"),
                        import_action(ACCEPT, "Accept a proposed document"),
                        import_action(DECLINE, "Decline a proposed document"),
                    ],
                },
            );
        let state = self.clone();
        tokio::task::spawn_local(async move {
            while let Some(envelope) = receiver.recv().await {
                let outcome = state
                    .execute_document_import(peer_id, &envelope, service.as_ref())
                    .await;
                let update = match outcome {
                    Ok(output) => tilo_actions::InvocationUpdate::Succeeded {
                        invocation: envelope.invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(output),
                            resources: Vec::new(),
                        },
                    },
                    Err(DesktopCommandError::Rejected(message)) => {
                        tilo_actions::InvocationUpdate::Failed {
                            invocation: envelope.invocation,
                            error: tilo_actions::ActionFailure {
                                code: "document_import_refused".into(),
                                details: None,
                                message,
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

    async fn execute_document_import(
        &self,
        peer_id: u64,
        envelope: &InvocationEnvelope,
        service: &dyn crate::services::DocumentImports,
    ) -> Result<serde_json::Value, DesktopCommandError> {
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
            .ok_or_else(|| Rejected("document import invocation is no longer admitted".into()))?;
        let key = staged
            .journal_key
            .as_ref()
            .ok_or_else(|| Rejected("document import has no durable admission".into()))?;
        // Only a person's certified device proposes or answers; an agent
        // task or an argument never stands in for one.
        let evidence = staged.network_evidence.as_ref().ok_or_else(|| {
            Rejected("document imports require a certified person's device".into())
        })?;
        let current = self
            .verify_network_action_caller_with_policy_time(evidence, staged.policy_time.as_ref())
            .map_err(Rejected)?;
        let caller = evidence.certificate().clone();
        if key.owner != current.owner()
            || key.room != service.room()
            || key.invocation != envelope.invocation
            || envelope.resource != RESOURCE
        {
            return Err(Rejected("document import binding changed".into()));
        }
        let action =
            ImportAction::parse(&envelope.action, &argument(envelope)?).map_err(Rejected)?;
        let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
            room: key.room.clone(),
            operation: tilo_peer_state::owner_commit::CommitOperation::DocumentImport {
                invocation: envelope.invocation.0,
                caller,
                action: action.clone(),
            },
        };
        let receipt = service.execute(request.clone()).await?;
        receipt.validate_for(&request).map_err(Uncertain)?;
        let (record, state) = match &action {
            ImportAction::Propose(argument) => (argument.proposal.record.clone(), "pending"),
            ImportAction::Accept(_) => (String::new(), "accepted"),
            ImportAction::Decline(_) => (String::new(), "declined"),
        };
        Ok(serde_json::json!({
            "proposal": import::key(&action.proposal_id()),
            "record": record,
            "state": state,
            "generation": receipt.generation,
        }))
    }
}

/// The typed argument text an import invocation carries.
fn argument(envelope: &InvocationEnvelope) -> Result<String, DesktopCommandError> {
    let raw = envelope
        .argument
        .as_deref()
        .ok_or_else(|| DesktopCommandError::Rejected("document import argument missing".into()))?;
    let raw = raw
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(raw);
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| DesktopCommandError::Rejected(e.to_string()))?;
    value
        .get(ARGUMENT)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            DesktopCommandError::Rejected("document import argument must be JSON text".into())
        })
}

fn import_action(action: &str, label: &str) -> PublishedAction {
    PublishedAction {
        semantic_identity: None,
        resource: RESOURCE.into(),
        action: action.into(),
        resource_kind: "document-import".into(),
        resource_label: "Proposed document".into(),
        action_label: label.into(),
        description: Some(label.into()),
        operation_kind: tilo_actions::OperationKind::Effect,
        params: vec![tilo_actions::ActionParam::required(
            ARGUMENT,
            tilo_actions::ParamType::Text,
            "The typed proposal or decision, as JSON text",
        )],
        contract: tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            result: tilo_actions::ResultSpec::Json {
                schema: outcome_schema(),
            },
            ..Default::default()
        },
        view: None,
        input: PublishedInput::None,
    }
}

fn outcome_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["proposal", "record", "state", "generation"],
        "properties": {
            "proposal": {"type": "string"},
            "record": {"type": "string"},
            "state": {"enum": ["pending", "accepted", "declined"]},
            "generation": {"type": "integer", "minimum": 1},
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_import_action_answers_with_its_published_outcome() {
        for action in [PROPOSE, ACCEPT, DECLINE] {
            let published = import_action(action, "label");
            let output = |value| tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            };
            let outcome = serde_json::json!({
                "proposal": "ab", "record": "", "state": "pending", "generation": 3,
            });
            assert_eq!(published.contract.result.validate(&output(outcome)), Ok(()));
            assert!(
                published
                    .contract
                    .result
                    .validate(&output(serde_json::json!({"state": "created"})))
                    .is_err()
            );
        }
    }
}
