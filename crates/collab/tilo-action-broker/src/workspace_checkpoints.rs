//! Checkpoints are ordinary room resources; arguments cannot supply caller custody.
use super::{InvocationEnvelope, PublishedAction, PublishedInput};
use tilo_peer_state::workspace_checkpoint::CheckpointCommand;
pub(super) const RESOURCE: &str = "desktop:checkpoints";

pub(super) fn intent(
    room: &str,
    caller: &tilo_user_identity::DeviceCert,
    envelope: &InvocationEnvelope,
) -> Result<serde_json::Value, String> {
    let command = command(envelope)?.ok_or("checkpoint command absent")?;
    let request = tilo_peer_state::workspace_checkpoint::CheckpointRequest {
        room: room.into(),
        invocation: envelope.invocation.0,
        caller: caller.clone(),
        command,
    };
    Ok(serde_json::json!({"checkpoint_request": request.digest()?}))
}

pub(super) fn command(envelope: &InvocationEnvelope) -> Result<Option<CheckpointCommand>, String> {
    if envelope.resource != RESOURCE {
        return Ok(None);
    }
    let encoded = envelope
        .argument
        .as_deref()
        .ok_or("checkpoint arguments missing")?;
    if encoded.len() > 160 * 1024 {
        return Err("checkpoint arguments exceed bounded size".into());
    }
    let encoded = encoded
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(encoded);
    let mut input: serde_json::Value = serde_json::from_str(encoded).map_err(|e| e.to_string())?;
    let object = input
        .as_object_mut()
        .ok_or("checkpoint arguments must be an object")?;
    if object.contains_key("command") {
        return Err("checkpoint arguments cannot override the action".into());
    }
    if !matches!(
        envelope.action.as_str(),
        "capture" | "inspect" | "name-bind" | "find"
    ) {
        return Err("unknown checkpoint action".into());
    }
    object.insert("command".into(), envelope.action.clone().into());
    let command: CheckpointCommand = serde_json::from_value(input).map_err(|e| e.to_string())?;
    command.validate()?;
    Ok(Some(command))
}

pub(super) fn actions() -> Vec<PublishedAction> {
    use tilo_actions::{ActionParam, ParamType};
    let name = || ActionParam::required("name", ParamType::Text, "Bounded checkpoint name");
    let id = || ActionParam::required("id", ParamType::Text, "Immutable checkpoint UUID");
    [
        (
            "capture",
            "Capture checkpoint",
            vec![
                name(),
                ActionParam::required(
                    "routes",
                    ParamType::Array,
                    "Sorted distinct native app/record routes",
                ),
            ],
            true,
        ),
        ("inspect", "Inspect checkpoint", vec![id()], false),
        (
            "name-bind",
            "Move checkpoint name",
            vec![name(), id()],
            true,
        ),
        ("find", "Find checkpoint by name", vec![name()], false),
    ]
    .into_iter()
    .map(|(action, label, params, mutation)| PublishedAction {
        semantic_identity: None,
        resource: RESOURCE.into(),
        action: action.into(),
        resource_kind: "workspace-checkpoint".into(),
        resource_label: "Native checkpoints".into(),
        action_label: label.into(),
        description: None,
        operation_kind: if mutation {
            tilo_actions::OperationKind::Document
        } else {
            tilo_actions::OperationKind::View
        },
        params,
        contract: tilo_actions::ActionContract {
            kind: if mutation {
                tilo_actions::ActionKind::Undoable
            } else {
                tilo_actions::ActionKind::Read
            },
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({"type":"object"}),
            },
            ..Default::default()
        },
        view: None,
        input: PublishedInput::None,
    })
    .collect()
}

impl super::CapnpBrokerState {
    fn authorize_checkpoint_request(
        &self,
        room: &str,
        caller: &tilo_actions::remote::Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<(), String> {
        let peer = request
            .target
            .provider
            .parse::<u64>()
            .map_err(|_| "checkpoint provider invalid")?;
        if request.on_behalf_of.is_some() {
            return Err("checkpoint delegation is not supported".into());
        }
        let surfaces = self.published_surfaces();
        let action = surfaces
            .iter()
            .find(|surface| {
                surface.peer_id == peer
                    && surface.app_id == "so.tilo.desktop"
                    && surface.route.cluster_id == room
            })
            .and_then(|surface| {
                surface.actions.iter().find(|action| {
                    action.resource == RESOURCE
                        && action.action == request.target.address.action.as_str()
                })
            })
            .ok_or("checkpoint provider changed")?;
        let now = self
            .registered_policy_time()?
            .ok_or("checkpoint policy time unavailable")?
            .now_ns()
            .map_err(|e| e.to_string())?
            / 1_000_000_000;
        self.authorize_invocation_permission(
            room,
            (
                "so.tilo.desktop",
                "workspace-checkpoint",
                RESOURCE,
                &action.action,
            ),
            &action.contract.need(),
            &self.caller_actor(caller, room),
            now,
        )?;
        Ok(())
    }

    pub(super) fn checkpoint_replay(
        &self,
        room: &str,
        certificate: &tilo_user_identity::DeviceCert,
        caller: &tilo_actions::remote::Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<Option<tilo_actions::remote::InvocationReceipt>, String> {
        if request.target.address.resource.as_str() != RESOURCE {
            return Ok(None);
        }
        self.authorize_checkpoint_request(room, caller, request)?;
        let envelope = InvocationEnvelope {
            id: 0,
            invocation: request.invocation,
            resource: RESOURCE.into(),
            action: request.target.address.action.clone(),
            argument: match &request.argument {
                tilo_actions::remote::InvokeArgument::Json(value) => Some(format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::to_string(value).map_err(|e| e.to_string())?
                )),
                _ => return Err("checkpoint arguments must be JSON".into()),
            },
            actor: String::new(),
            context: None,
        };
        let intent = intent(room, certificate, &envelope)?;
        let owner = super::InvocationOwner::Local {
            actor: super::principal_id_for_root(certificate.user_public_key),
        };
        let record = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| "checkpoint journal unavailable")?;
            let Some(journal) = guard.as_ref().map_err(Clone::clone)?.as_ref() else {
                return Ok(None);
            };
            let key = super::InvocationKey {
                room: room.into(),
                host: journal.host(),
                owner: owner.clone(),
                invocation: request.invocation,
            };
            let record = journal.lookup(&key)?;
            drop(guard);
            record
        };
        let Some(record) = record else {
            return Ok(None);
        };
        if !record.admitted_request_is(&intent) {
            return Err("checkpoint invocation belongs to another command or caller".into());
        }
        self.authorize_checkpoint_result(room, Some(certificate), &record)?;
        let mut ledger = self
            .remote_invocations
            .lock()
            .map_err(|_| "checkpoint ledger unavailable")?;
        ledger.reserve(caller, request).map_err(|e| e.to_string())?;
        ledger.complete(&request.invocation, record.wire_id);
        drop(ledger);
        self.record_completion(record.wire_id, owner, record.update.clone());
        Ok(Some(
            tilo_actions::remote::InvocationState::new(request.invocation, record.update).receipt(),
        ))
    }

    pub(super) fn authorize_checkpoint_result(
        &self,
        room: &str,
        certificate: Option<&tilo_user_identity::DeviceCert>,
        record: &super::DurableInvocation,
    ) -> Result<(), String> {
        if record
            .intent
            .get("app_id")
            .and_then(serde_json::Value::as_str)
            != Some("so.tilo.desktop")
            || record
                .intent
                .get("resource")
                .and_then(serde_json::Value::as_str)
                != Some(RESOURCE)
        {
            return Ok(());
        }
        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = &record.update else {
            return Ok(());
        };
        let id = output
            .value
            .as_ref()
            .and_then(|value| value.get("id"))
            .and_then(serde_json::Value::as_str)
            .ok_or("checkpoint result identity missing")?;
        let id = uuid::Uuid::parse_str(id).map_err(|_| "checkpoint result identity invalid")?;
        let certificate = certificate.ok_or("checkpoint result caller custody unavailable")?;
        let action = record
            .intent
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or("checkpoint result action missing")?;
        self.services
            .workspace_memberships
            .as_ref()
            .ok_or("checkpoint service unavailable")?
            .checkpoint_result_access(
                room,
                certificate,
                id,
                matches!(action, "capture" | "name-bind"),
            )
    }

    pub(super) fn authorize_checkpoint_job(
        &self,
        room: &str,
        certificate: Option<&tilo_user_identity::DeviceCert>,
        job: u64,
    ) -> Result<(), String> {
        let Some(update) = self.invocation_status(job) else {
            return Ok(());
        };
        if !matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }) {
            return Ok(());
        }
        let owner = self
            .pending_invocations
            .lock()
            .map_err(|_| "checkpoint invocation registry unavailable")?
            .get(&job)
            .map(|pending| pending.owner.clone());
        let owner = match owner {
            Some(owner) => owner,
            None => self
                .completed_invocations
                .lock()
                .map_err(|_| "checkpoint completed registry unavailable")?
                .get(&job)
                .map(|(owner, _)| owner.clone())
                .ok_or("checkpoint invocation owner unavailable")?,
        };
        let record = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| "checkpoint journal unavailable")?;
            let Some(journal) = guard.as_ref().map_err(Clone::clone)?.as_ref() else {
                return Ok(());
            };
            let key = super::InvocationKey {
                room: room.into(),
                host: journal.host(),
                owner,
                invocation: update.invocation(),
            };
            let record = journal.lookup(&key)?;
            drop(guard);
            record
        };
        if let Some(record) = record {
            self.authorize_checkpoint_result(room, certificate, &record)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn envelope(action: &str, argument: &serde_json::Value) -> InvocationEnvelope {
        InvocationEnvelope {
            id: 1,
            invocation: tilo_actions::InvocationId::new(),
            resource: RESOURCE.into(),
            action: action.into(),
            argument: Some(format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                argument
            )),
            actor: "not-authority".into(),
            context: None,
        }
    }
    #[test]
    fn checkpoint_arguments_are_closed_bounded_and_never_authority() {
        let valid = serde_json::json!({"name":"first", "routes":[{"app":"so.tilo.chat", "record":"thread"}]});
        assert!(matches!(
            command(&envelope("capture", &valid)).unwrap(),
            Some(CheckpointCommand::Capture { .. })
        ));
        for field in [
            "caller",
            "room",
            "signing_key",
            "governor",
            "invocation",
            "command",
            "agent",
            "author",
        ] {
            let mut forged = valid.clone();
            forged[field] = "forged".into();
            assert!(command(&envelope("capture", &forged)).is_err(), "{field}");
        }
        for invalid in [
            serde_json::json!({"name":"first", "routes":[]}),
            serde_json::json!({"name":"first", "routes":[{"app":"so.tilo.desktop", "record":"membership"}]}),
            serde_json::json!({"name":"first", "routes":[{"app":"so.tilo.chat", "record":"b"},{"app":"so.tilo.chat", "record":"a"}]}),
            serde_json::json!({"name":"x".repeat(257), "routes":[{"app":"so.tilo.chat", "record":"a"}]}),
        ] {
            assert!(command(&envelope("capture", &invalid)).is_err());
        }
        assert!(
            command(&envelope(
                "inspect",
                &serde_json::json!({"id":uuid::Uuid::nil()})
            ))
            .is_err()
        );
        assert!(command(&envelope("unknown", &serde_json::json!({}))).is_err());
    }
    #[test]
    fn checkpoint_discovery_declares_read_and_undoable_write_separately() {
        let actions = actions();
        assert_eq!(actions.len(), 4);
        for action in actions {
            assert_eq!(action.resource, RESOURCE);
            let mutation = matches!(action.action.as_str(), "capture" | "name-bind");
            assert_eq!(
                action.contract.kind,
                if mutation {
                    tilo_actions::ActionKind::Undoable
                } else {
                    tilo_actions::ActionKind::Read
                }
            );
        }
    }
}
