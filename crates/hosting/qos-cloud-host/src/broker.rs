//! Canonical certified-action adapter for the production Cloud host.

use crate::agent_owner::RuntimePolicyClock;
use crate::{ActionReceipt, ActionRequest, NativeOperation, Store, now_time};
use anyhow::{Result, anyhow, bail};
use qos_action_broker::collaboration_admin as shared;
use qos_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use qos_peer_state::join::MemberRole;
use qos_relay::peer_actions::{ActionMessage, DirectedAction, ExactActionRoute};
use qos_user_identity::DeviceCert;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
type RetainedPolicy = (
    Arc<RuntimePolicyClock>,
    qos_action_broker::peer_actions::PolicyTime,
);

fn open_store(
    root: &std::path::Path,
    machine: &str,
    policy: Option<&RetainedPolicy>,
) -> Result<Store> {
    match policy {
        Some((owner, time)) => owner
            .open_store(root, machine, time)
            .map_err(anyhow::Error::msg),
        None => Store::open(root, machine),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredCanonicalIntent {
    digest: [u8; 32],
    operation: NativeOperation,
}

fn host_id(store: &Store) -> Result<uuid::Uuid> {
    store.ensure_reconciled()?;
    Ok(store.state.room.parse()?)
}

fn parse_role(value: &serde_json::Value) -> Result<MemberRole> {
    match value.as_str() {
        Some("viewer") => Ok(MemberRole::Viewer),
        Some("commenter") => Ok(MemberRole::Commenter),
        Some("editor") => Ok(MemberRole::Editor),
        _ => bail!("invalid collaboration role"),
    }
}

fn parse_key(value: &serde_json::Value, field: &str) -> Result<[u8; 32]> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("{field} must be a lower-hex person identity"))?;
    if text.len() != 64 || text.bytes().any(|byte| !byte.is_ascii_hexdigit()) {
        bail!("{field} must be a 32-byte person identity")
    }
    let bytes = hex::decode(text)?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("{field} must be a 32-byte person identity"))
}

fn descriptor(
    resource: &str,
    definition: &shared::CollaborationActionDefinition,
    revision: &str,
) -> serde_json::Value {
    serde_json::json!({
        "resource": resource,
        "action": definition.id,
        "revision": revision,
        "resource_kind": "workspace",
        "resource_label": "Workspace collaboration",
        "action_label": definition.label,
        "description": definition.description,
        "operation_kind": qos_actions::OperationKind::Effect,
        "params": definition.params,
        "contract": definition.contract,
    })
}

fn catalogue(
    store: &Store,
    source: [u8; 32],
    scope: Option<&ExactActionRoute>,
    now: u64,
) -> Result<serde_json::Value> {
    store.ensure_reconciled()?;
    let peer = store.peer_at(now)?;
    let (role, caps, person) = peer
        .permissions(source, now_time(now))
        .map_err(anyhow::Error::msg)?;
    let resource = format!("workspace:{}", store.state.room);
    let revision = store.state.generation.to_string();
    let mut actions = if role == collab_types::Role::Owner {
        shared::action_definitions()
            .iter()
            .map(|definition| descriptor(&resource, definition, &revision))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if store.state.version == crate::delivery::VERSION && peer.admission_v5_selected() {
        use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
        let actor = Actor::person(Some(role), &person).in_room(store.state.room.clone());
        if decide_capability(
            &actor,
            &ResourceRef::Room(store.state.room.clone()),
            Verb::Admin,
            &caps,
            Some(now / 1_000_000_000),
        ) == CapDecision::Allow
        {
            actions.extend(
                shared::media_action_definitions()
                    .iter()
                    .map(|definition| descriptor(&resource, definition, &revision)),
            );
        }
    }
    // The workspace desktop: open an application (people's launcher and
    // agents' published open action) and the browser's layout commands.
    let mut desktop = crate::desktop_commands::desktop_descriptors(&revision);
    if let Some(scope) = scope {
        let keep = |app: &str, descriptor: &serde_json::Value| {
            scope.app_id == app
                && scope.record_id.is_empty()
                && descriptor["resource"] == scope.resource
                && descriptor["action"] == scope.action
        };
        actions.retain(|descriptor| keep(shared::APP_ID, descriptor));
        desktop.retain(|descriptor| keep(qos_peer_state::desktop_command::APP_ID, descriptor));
    }
    let peer_id = qos_relay::room_workspace_id(&store.state.room)?;
    let mut surfaces = vec![];
    if !actions.is_empty() || scope.is_none() {
        surfaces.push(serde_json::json!({
            "peer_id": peer_id,
            "app_id": shared::APP_ID,
            "cluster_id": store.state.room,
            "record_id": "",
            "live": true,
            "actions": actions,
        }));
    }
    if !desktop.is_empty() {
        surfaces.push(serde_json::json!({
            "peer_id": peer_id,
            "app_id": qos_peer_state::desktop_command::APP_ID,
            "cluster_id": store.state.room,
            "record_id": "",
            "live": true,
            "actions": desktop,
        }));
    }
    Ok(serde_json::json!({"version": 1, "surfaces": surfaces}))
}

fn canonical_operation(
    store: &mut Store,
    source: [u8; 32],
    invocation: InvocationId,
    action: &str,
    argument: Option<&serde_json::Value>,
    now: u64,
) -> Result<NativeOperation> {
    store.ensure_reconciled()?;
    if store
        .private_agent_invocation_reserved(source, invocation.0)
        .map_err(anyhow::Error::msg)?
    {
        bail!("same-caller invocation is already reserved by the private agent owner")
    }
    let intent = serde_json::json!({"action":action,"argument":argument});
    let digest = *blake3::hash(&serde_json::to_vec(&intent)?).as_bytes();
    let scope = format!("{}:{}", hex::encode(source), invocation.0);
    if let Some(prior) = store.state.canonical_intents.get(&scope) {
        if prior.digest != digest {
            bail!("invocation substitution")
        }
        return Ok(prior.operation.clone());
    }
    let argument = argument
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let operation = match action {
        "invitation.issue" => {
            let role = parse_role(argument.get("role").unwrap_or(&serde_json::Value::Null))?;
            let seconds = argument
                .get("expires_in_seconds")
                .and_then(serde_json::Value::as_u64)
                .filter(|seconds| *seconds > 0)
                .ok_or_else(|| anyhow!("invalid invitation lifetime"))?;
            let expires_at_ns = now
                .checked_add(
                    seconds
                        .checked_mul(1_000_000_000)
                        .ok_or_else(|| anyhow!("invitation lifetime overflow"))?,
                )
                .ok_or_else(|| anyhow!("invitation expiry overflow"))?;
            NativeOperation::CollaborationInvite {
                role,
                recipient_user_public_key: None,
                invitee_name: argument
                    .get("invitee_name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                expires_at_ns,
            }
        }
        "invitation.cancel" => NativeOperation::CollaborationCancelInvitation {
            invite_id: parse_key(
                argument
                    .get("invite_id")
                    .unwrap_or(&serde_json::Value::Null),
                "invite_id",
            )?,
        },
        "invitation.list" => NativeOperation::CollaborationListInvitations,
        "member.role.set" => NativeOperation::CollaborationSetRole {
            user_public_key: parse_key(
                argument
                    .get("participant_id")
                    .unwrap_or(&serde_json::Value::Null),
                "participant_id",
            )?,
            role: parse_role(argument.get("role").unwrap_or(&serde_json::Value::Null))?,
        },
        "member.remove" => NativeOperation::CollaborationRemove {
            user_public_key: parse_key(
                argument
                    .get("participant_id")
                    .unwrap_or(&serde_json::Value::Null),
                "participant_id",
            )?,
        },
        shared::MEDIA_CREATE_ACTION => {
            NativeOperation::MediaCreate(serde_json::from_value(argument.into())?)
        }
        shared::MEDIA_END_ACTION => {
            NativeOperation::MediaEnd(serde_json::from_value(argument.into())?)
        }
        shared::MEDIA_RESTART_ACTION => {
            NativeOperation::MediaRestart(serde_json::from_value(argument.into())?)
        }
        shared::MEDIA_DIRECTIONS_ACTION => {
            NativeOperation::MediaSetDirections(serde_json::from_value(argument.into())?)
        }
        shared::MEDIA_DENIAL_ACTION => {
            NativeOperation::MediaSetParticipantDenial(serde_json::from_value(argument.into())?)
        }
        _ => bail!("unsupported collaboration action"),
    };
    let mut state = store.state.clone();
    state.canonical_intents.insert(
        scope,
        StoredCanonicalIntent {
            digest,
            operation: operation.clone(),
        },
    );
    store.commit(state)?;
    Ok(operation)
}

fn operation_wire(
    operation: &shared::CollaborationAdminOperation,
) -> Result<(&'static str, serde_json::Value)> {
    Ok(match operation {
        shared::CollaborationAdminOperation::Issue(input) => {
            (shared::ISSUE_ACTION, serde_json::to_value(input)?)
        }
        shared::CollaborationAdminOperation::Cancel(input) => {
            (shared::CANCEL_ACTION, serde_json::to_value(input)?)
        }
        shared::CollaborationAdminOperation::List => (shared::LIST_ACTION, serde_json::json!({})),
        shared::CollaborationAdminOperation::SetRole(input) => {
            (shared::SET_ROLE_ACTION, serde_json::to_value(input)?)
        }
        shared::CollaborationAdminOperation::Remove(input) => {
            (shared::REMOVE_ACTION, serde_json::to_value(input)?)
        }
        shared::CollaborationAdminOperation::MediaCreate(v) => {
            (shared::MEDIA_CREATE_ACTION, serde_json::to_value(v)?)
        }
        shared::CollaborationAdminOperation::MediaEnd(v) => {
            (shared::MEDIA_END_ACTION, serde_json::to_value(v)?)
        }
        shared::CollaborationAdminOperation::MediaRestart(v) => {
            (shared::MEDIA_RESTART_ACTION, serde_json::to_value(v)?)
        }
        shared::CollaborationAdminOperation::MediaSetDirections(v) => {
            (shared::MEDIA_DIRECTIONS_ACTION, serde_json::to_value(v)?)
        }
        shared::CollaborationAdminOperation::MediaSetParticipantDenial(v) => {
            (shared::MEDIA_DENIAL_ACTION, serde_json::to_value(v)?)
        }
    })
}

#[derive(Clone)]
pub(crate) struct CloudCollaborationAdmin {
    root: std::path::PathBuf,
    machine: String,
    room: String,
    policy: Option<RetainedPolicy>,
}

impl CloudCollaborationAdmin {
    pub(crate) fn new(root: &std::path::Path, machine: &str, room: &str) -> Self {
        Self {
            root: root.into(),
            machine: machine.into(),
            room: room.into(),
            policy: None,
        }
    }
    fn with_policy(mut self, policy: Option<RetainedPolicy>) -> Self {
        self.policy = policy;
        self
    }
    fn open_store(&self) -> Result<Store> {
        open_store(&self.root, &self.machine, self.policy.as_ref())
    }

    fn execute_sync(
        &self,
        request: shared::CollaborationAdminRequest,
    ) -> Result<serde_json::Value> {
        let mut store = self.open_store()?;
        let now = store.policy_now_ns()?;
        if request.room != self.room || request.room != store.state.room {
            bail!("collaboration request room mismatch")
        }
        let peer = store.peer_at(now)?;
        if !peer
            .certificates()
            .iter()
            .any(|known| known == &request.caller)
        {
            bail!("collaboration caller certificate is not current")
        }
        let (action_name, argument) = operation_wire(&request.operation)?;
        let operation = canonical_operation(
            &mut store,
            request.caller.device_public_key,
            request.invocation,
            action_name,
            Some(&argument),
            now,
        )?;
        let expires_at_ms = match &operation {
            NativeOperation::CollaborationInvite { expires_at_ns, .. } => {
                Some(*expires_at_ns / 1_000_000)
            }
            _ => None,
        };
        let receipt = store.action(
            request.caller.device_public_key,
            &ActionRequest {
                version: 1,
                room: store.state.room.clone(),
                invocation_id: request.invocation.0.to_string(),
                operation,
            },
            now,
        )?;
        if receipt.status != "completed" {
            bail!(
                receipt
                    .error
                    .unwrap_or_else(|| "collaboration action failed".into())
            )
        }
        let output = receipt.output.unwrap_or_else(|| serde_json::json!({}));
        Ok(match action_name {
            shared::ISSUE_ACTION => serde_json::json!({
                "invite": output.get("signed_invitation").cloned().unwrap_or(serde_json::Value::Null),
                "invite_id": output.get("invite_id").cloned().unwrap_or(serde_json::Value::Null),
                "expires_at_ms": expires_at_ms,
            }),
            shared::SET_ROLE_ACTION | shared::REMOVE_ACTION => {
                serde_json::json!({"generation":store.state.generation.to_string()})
            }
            _ => output,
        })
    }
}

impl shared::CollaborationAdmin for CloudCollaborationAdmin {
    fn room(&self) -> &str {
        &self.room
    }

    fn supports_media_governance(&self) -> bool {
        // This is the existing action writer capability, not HTTP installation
        // or allocation qualification. Ordinary legacy open never selects V6.
        let opened = match self.policy.as_ref() {
            Some(_) => self
                .open_store()
                .map_err(crate::delivery::access::AccessError::Denied),
            None => crate::delivery::access::try_open(&self.root, &self.machine),
        };
        opened
            .and_then(|store| {
                if store.state.room != self.room {
                    return Err(crate::delivery::access::AccessError::Denied(anyhow!(
                        "room mismatch"
                    )));
                }
                crate::delivery::committed_peer(
                    &store.state,
                    store
                        .policy_now_ns()
                        .map_err(crate::delivery::access::AccessError::Denied)?,
                )
                .map(|peer| peer.admission_v5_selected() && peer.incomplete_reason().is_none())
                .map_err(crate::delivery::access::AccessError::Denied)
            })
            .unwrap_or(false)
    }

    fn execute(
        &self,
        request: shared::CollaborationAdminRequest,
    ) -> shared::CollaborationAdminFuture {
        let service = self.clone();
        Box::pin(async move {
            service.execute_sync(request).map_err(|error| {
                if crate::is_store_failure(&error) {
                    shared::CollaborationAdminError::Uncertain(error.to_string())
                } else if let Some(semantic) =
                    error.downcast_ref::<shared::CollaborationAdminError>()
                {
                    semantic.clone()
                } else {
                    shared::CollaborationAdminError::Rejected(error.to_string())
                }
            })
        })
    }
}

fn update_from_receipt(
    invocation: InvocationId,
    action: &str,
    receipt: ActionReceipt,
    generation: u64,
) -> InvocationUpdate {
    if receipt.status == "completed" {
        let mut value = receipt.output.unwrap_or_else(|| serde_json::json!({}));
        if action == "invitation.issue" {
            value = serde_json::json!({
                "invite": value.get("signed_invitation").cloned().unwrap_or(serde_json::Value::Null),
                "invite_id": value.get("invite_id").cloned().unwrap_or(serde_json::Value::Null),
                "expires_at_ms": value.get("expires_at_ns").and_then(serde_json::Value::as_u64).map(|value| value / 1_000_000),
            });
        } else if matches!(action, "member.role.set" | "member.remove") {
            value = serde_json::json!({"generation":generation.to_string()});
        }
        InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        }
    } else if receipt.status == "cancelled" {
        InvocationUpdate::Cancelled {
            invocation,
            message: receipt.error,
        }
    } else {
        InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: "collaboration_action_rejected".into(),
                message: receipt
                    .error
                    .unwrap_or_else(|| "collaboration action failed".into()),
                details: None,
                retryable: false,
            },
        }
    }
}

fn failed(invocation: InvocationId, error: impl ToString) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: "collaboration_action_rejected".into(),
            message: error.to_string(),
            details: None,
            retryable: false,
        },
    }
}

fn observation(
    host: uuid::Uuid,
    invocation: InvocationId,
    update: InvocationUpdate,
) -> ActionMessage {
    ActionMessage::Observation {
        host,
        invocation,
        update: Some(update),
        uncertain: false,
        cancellation_requested: false,
    }
}

pub(crate) async fn handle(
    root: &std::path::Path,
    machine: &str,
    certificate: &DeviceCert,
    action: &DirectedAction,
    now: u64,
) -> Result<ActionMessage> {
    handle_inner(root, machine, certificate, action, now, None).await
}
pub(crate) async fn handle_with_policy_time(
    root: &std::path::Path,
    machine: &str,
    certificate: &DeviceCert,
    action: &DirectedAction,
    time: &qos_action_broker::peer_actions::PolicyTime,
    owner: Arc<RuntimePolicyClock>,
) -> Result<ActionMessage> {
    let policy = (owner, time.clone());
    handle_inner(
        root,
        machine,
        certificate,
        action,
        time.now_ns()?,
        Some(&policy),
    )
    .await
}
async fn handle_inner(
    root: &std::path::Path,
    machine: &str,
    certificate: &DeviceCert,
    action: &DirectedAction,
    now: u64,
    policy: Option<&RetainedPolicy>,
) -> Result<ActionMessage> {
    let result: Result<ActionMessage> = async {
        let store = open_store(root, machine, policy)?;
        let now = store.policy_now_or(now)?;
        let host = host_id(&store)?;
        let peer = store.peer_at(now)?;
        if action.room != host
            || action.source != certificate.device_public_key
            || action.target != peer.authority().device_public_key
            || !peer.certificates().iter().any(|known| known == certificate)
        {
            bail!("certified collaboration action identity mismatch")
        }
        peer.permissions(action.source, now_time(now))
            .map_err(anyhow::Error::msg)?;
        match &action.message {
            ActionMessage::CatalogueRefresh { scope } => Ok(ActionMessage::BrokerCatalogue {
                host,
                catalogue: catalogue(&store, action.source, scope.as_ref(), now)?,
            }),
            ActionMessage::InvokeBound {
                host: requested_host,
                invocation,
                app_id,
                record_id,
                resource,
                action: action_name,
                argument,
                ..
            } if app_id == qos_peer_state::desktop_command::APP_ID => {
                if *requested_host != host || !record_id.is_empty() {
                    bail!("desktop action route mismatch")
                }
                let request = crate::desktop_commands::person_request(
                    &store.state.room,
                    *invocation,
                    certificate,
                    resource,
                    action_name,
                    argument.as_ref(),
                )?;
                drop(store);
                let mut store = open_store(root, machine, policy)?;
                // Receipt or refusal, both carrying the desktop command id.
                let update = store.desktop_command_update(&request, None)?;
                Ok(observation(host, *invocation, update))
            }
            ActionMessage::InvokeBound {
                host: requested_host,
                invocation,
                app_id,
                record_id,
                resource,
                action: action_name,
                argument,
                ..
            } => {
                if *requested_host != host
                    || app_id != shared::APP_ID
                    || !record_id.is_empty()
                    || resource != &shared::resource(&store.state.room)
                {
                    bail!("collaboration action route mismatch")
                }
                let argument_json = argument.as_ref().map(serde_json::Value::to_string);
                let operation = shared::parse_operation(action_name, argument_json.as_deref())
                    .map_err(anyhow::Error::msg)?;
                drop(store);
                let service = CloudCollaborationAdmin::new(root, machine, &action.room.to_string())
                    .with_policy(policy.cloned());
                // Preserve the Store cause through this local synchronous adapter.
                let value = service.execute_sync(shared::CollaborationAdminRequest {
                    room: action.room.to_string(),
                    invocation: *invocation,
                    caller: certificate.clone(),
                    operation,
                })?;
                let update = InvocationUpdate::Succeeded {
                    invocation: *invocation,
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                };
                Ok(observation(host, *invocation, update))
            }
            ActionMessage::Status { invocation } => {
                let scope = format!("{}:{}", hex::encode(action.source), invocation.0);
                let unadmitted = !store.state.receipts.contains_key(&scope)
                    && !store.state.canonical_intents.contains_key(&scope)
                    && !store.state.pending_models.contains_key(&scope);
                // Never admitted and past its deadline: fence it, then answer
                // definitively (see `unadmitted`).
                if unadmitted
                    && unadmitted::deadline_passed(&store.state.room, action.source, invocation.0)
                {
                    drop(store);
                    let mut fenced = open_store(root, machine, policy)?;
                    let definitive = unadmitted::fence(&mut fenced, action.source, *invocation)?;
                    return Ok(ActionMessage::Observation {
                        host,
                        invocation: *invocation,
                        update: definitive.then(|| unadmitted::not_found_update(*invocation)),
                        uncertain: false,
                        cancellation_requested: false,
                    });
                }
                let update = store.state.receipts.get(&scope).map(|(_, receipt)| {
                    if receipt.status == unadmitted::NOT_FOUND {
                        return unadmitted::not_found_update(*invocation);
                    }
                    let action_name = store
                        .state
                        .canonical_intents
                        .get(&scope)
                        .map(|intent| match intent.operation {
                            NativeOperation::CollaborationInvite { .. } => "invitation.issue",
                            NativeOperation::CollaborationCancelInvitation { .. } => {
                                "invitation.cancel"
                            }
                            NativeOperation::CollaborationListInvitations => "invitation.list",
                            NativeOperation::CollaborationSetRole { .. } => "member.role.set",
                            NativeOperation::CollaborationRemove { .. } => "member.remove",
                            _ => "unknown",
                        })
                        .unwrap_or("unknown");
                    update_from_receipt(
                        *invocation,
                        action_name,
                        receipt.clone(),
                        store.state.generation,
                    )
                });
                Ok(ActionMessage::Observation {
                    host,
                    invocation: *invocation,
                    update,
                    uncertain: false,
                    cancellation_requested: false,
                })
            }
            ActionMessage::Cancel { invocation } => Ok(ActionMessage::CancellationRequested {
                invocation: *invocation,
            }),
            _ => bail!("unsupported collaboration broker message"),
        }
    }
    .await;
    match result {
        Ok(response) => Ok(response),
        Err(error) if crate::is_store_failure(&error) => Err(error),
        Err(error) => Ok(action
            .message
            .invocation()
            .map(|invocation| {
                let host = open_store(root, machine, policy)
                    .and_then(|store| host_id(&store))
                    .unwrap_or_else(|_| uuid::Uuid::nil());
                observation(host, invocation, failed(invocation, error))
            })
            .unwrap_or(ActionMessage::Rejected {
                code: "collaboration_action_rejected".into(),
            })),
    }
}

pub(crate) fn reply(request: &DirectedAction, message: ActionMessage) -> DirectedAction {
    DirectedAction {
        room: request.room,
        request_id: request.request_id,
        source: request.target,
        target: request.source,
        message,
    }
}

mod unadmitted;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_owner_cross_journal_same_caller_reserved_and_other_caller_distinct() {
        let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
        let caller = store.agent_caller(device).unwrap();
        store.activate_private_agent_ledger(&caller).unwrap();
        let invocation = uuid::Uuid::from_u128(61);
        let (caller, request) =
            crate::agent_owner_tests::prepare(&store, device, &invocation.to_string());
        store.commit_agent_request(&request, &caller).unwrap();
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        assert!(
            canonical_operation(
                &mut store,
                device,
                InvocationId(invocation),
                "invitation.list",
                None,
                11
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
        let other_device = [19; 32];
        assert!(
            canonical_operation(
                &mut store,
                other_device,
                InvocationId(invocation),
                "invitation.list",
                None,
                11
            )
            .is_ok()
        );
        store
            .activate_private_agent_runtime_v2(&store.agent_caller(device).unwrap())
            .unwrap();
    }

    #[test]
    fn agent_owner_cross_journal_existing_conflict_refuses_v2_selection() {
        let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
        let invocation = uuid::Uuid::from_u128(62);
        canonical_operation(
            &mut store,
            device,
            InvocationId(invocation),
            "invitation.list",
            None,
            11,
        )
        .unwrap();
        store
            .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
            .unwrap();
        let (caller, request) =
            crate::agent_owner_tests::prepare(&store, device, &invocation.to_string());
        store.commit_agent_request(&request, &caller).unwrap();
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        assert!(
            store
                .activate_private_agent_runtime_v2(&store.agent_caller(device).unwrap())
                .is_err()
        );
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
    }

    #[test]
    fn role_and_person_arguments_are_canonical() {
        assert_eq!(
            parse_role(&serde_json::json!("editor")).unwrap(),
            MemberRole::Editor
        );
        assert_eq!(
            parse_key(&serde_json::json!("07".repeat(32)), "participant_id").unwrap(),
            [7; 32]
        );
        assert!(parse_key(&serde_json::json!("person-b"), "participant_id").is_err());
    }
}
