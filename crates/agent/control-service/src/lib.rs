//! Durable agent admission and status over one existing host owner.
//! This package supplies no process executor, transport route, or recovery dispatch permit.
use qos_action_broker::{policy::WorkspacePolicy, services::WorkspaceAuthority};
pub use qos_agent_control_protocol::{AgentError, AgentErrorCode, AgentOperation, SpawnInput};
use qos_agent_control_protocol::{
    AgentOperationState, AgentRequest, GrantAction, GrantOffer, canonical_request,
};
use qos_peer_state::owner_commit::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub trait AgentAuthority: WorkspaceAuthority {
    fn current(
        &self,
        caller: &VerifiedAgentCaller,
    ) -> Result<(VerifiedAgentCaller, AgentStateSnapshot), String>;
}
pub struct GrantCatalogueSnapshot {
    pub revision: String,
    pub actions: Vec<GrantAction>,
}
/// The catalogue is injected by the host. Browser aliases cannot construct publication custody.
pub trait GrantCatalogue: Send + Sync {
    fn current(&self, caller: &VerifiedAgentCaller) -> Result<GrantCatalogueSnapshot, AgentError>;
}
#[derive(Clone, Debug)]
pub struct PreparedAgent {
    pub ids: AgentIds,
    pub request_digest: [u8; 32],
}
struct HeldOffer {
    device: [u8; 32],
    offer: GrantOffer,
    expires: u64,
}
pub struct AgentAdmission {
    authority: Arc<dyn AgentAuthority>,
    owner: Arc<dyn OwnerCommitService>,
    catalogue: Arc<dyn GrantCatalogue>,
    offers: Mutex<BTreeMap<String, HeldOffer>>,
}
fn error(code: AgentErrorCode, message: impl Into<String>) -> AgentError {
    AgentError {
        code,
        message: message.into().chars().take(4096).collect(),
    }
}
/// Admission of one frozen scope for the current owner (MUST-1..4):
/// manifest-delegable app (no hard-coded app list), canonical non-reserved
/// records only, no workspace wildcard without a separate consent path, and
/// the owner holds the scope's verb on the app, on each exact action and on
/// each record's document artifact.
pub fn admissible_scope(
    workspace: &qos_desktop::Workspace,
    actor: &collab_types::caps::Actor,
    room: &str,
    registry: &qos_agent_control_protocol::DelegationRegistry,
    scope: &qos_agent_control_protocol::GrantScope,
) -> bool {
    use collab_types::caps::{ResourceRef, Verb};
    use qos_agent_control_protocol::{GrantVerb, RecordScope, route_record};
    if !registry.is_delegable_app(&scope.app_id) || matches!(scope.record, RecordScope::InWorkspace)
    {
        return false;
    }
    let verb = match scope.verb {
        GrantVerb::Read => Verb::Read,
        GrantVerb::Comment => Verb::Comment,
        GrantVerb::Write => Verb::Write,
    };
    let mut resources = vec![];
    match &scope.actions {
        Some(actions) => resources.extend(actions.iter().map(|action| ResourceRef::Action {
            app: scope.app_id.clone(),
            action: action.clone(),
        })),
        None => resources.push(ResourceRef::App(scope.app_id.clone())),
    }
    for record in scope.records() {
        let Ok(record) = route_record(record) else {
            return false;
        };
        resources.push(ResourceRef::Artifact {
            app: Some(scope.app_id.clone()),
            kind: collab_types::ArtifactKind::Note,
            instance: collab_types::document_artifact_id(&scope.app_id, room, record),
        });
    }
    resources
        .iter()
        .all(|resource| workspace.allows(actor, resource, verb))
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl AgentAdmission {
    pub fn new(
        authority: Arc<dyn AgentAuthority>,
        owner: Arc<dyn OwnerCommitService>,
        catalogue: Arc<dyn GrantCatalogue>,
    ) -> Self {
        Self {
            authority,
            owner,
            catalogue,
            offers: Mutex::new(BTreeMap::new()),
        }
    }
    fn current(
        &self,
        caller: &VerifiedAgentCaller,
    ) -> Result<(VerifiedAgentCaller, AgentStateSnapshot), AgentError> {
        self.authority
            .current(caller)
            .map_err(|e| error(AgentErrorCode::PermissionDenied, e))
    }
    fn current_offer(
        &self,
        caller: &VerifiedAgentCaller,
    ) -> Result<(String, Vec<GrantAction>), AgentError> {
        let (fresh, snapshot) = self.current(caller)?;
        let catalogue = self.catalogue.current(&fresh)?;
        // v1 exact tuples or v2 scope carriers; the scope count (at most
        // MAX_GRANT_SCOPES) replaces the fixed exact-tuple cap.
        let scopes =
            qos_agent_control_protocol::scopes_from_grants(&catalogue.actions).map_err(|_| {
                error(
                    AgentErrorCode::NoDelegableActions,
                    "No bounded delegable grant offer is available",
                )
            })?;
        let tree = self
            .authority
            .snapshot()
            .map_err(|e| error(AgentErrorCode::PermissionDenied, e))?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|w| w.room.to_string() == fresh.room())
            .ok_or_else(|| error(AgentErrorCode::PermissionDenied, "Workspace unavailable"))?;
        if workspace.owner.as_deref() != Some(fresh.person()) {
            return Err(error(
                AgentErrorCode::PermissionDenied,
                "Current owner required",
            ));
        }
        let actor = workspace.actor_for_person(&fresh.person().to_string());
        let registry = qos_agent_control_protocol::DelegationRegistry::first_party();
        for scope in &scopes {
            if !admissible_scope(workspace, &actor, fresh.room(), &registry, scope) {
                return Err(error(
                    AgentErrorCode::PermissionDenied,
                    "Selected action is not currently delegable",
                ));
            }
        }
        let bytes = serde_json::to_vec(&(
            snapshot.governance_revision,
            catalogue.revision,
            &catalogue.actions,
        ))
        .map_err(|e| error(AgentErrorCode::InternalError, e.to_string()))?;
        Ok((blake3::hash(&bytes).to_hex().to_string(), catalogue.actions))
    }
    pub fn offers(&self, caller: &VerifiedAgentCaller) -> Result<Vec<GrantOffer>, AgentError> {
        let (revision, actions) = self.current_offer(caller)?;
        let time = now();
        let mut offers = self
            .offers
            .lock()
            .map_err(|_| error(AgentErrorCode::InternalError, "Offer registry unavailable"))?;
        offers.retain(|_, o| o.expires > time);
        if let Some(existing) = offers
            .values()
            .find(|o| o.device == caller.device() && o.offer.revision == revision)
        {
            return Ok(vec![existing.offer.clone()]);
        }
        if offers.len() >= 64 {
            return Err(error(
                AgentErrorCode::CapacityReached,
                "Grant offer capacity reached",
            ));
        }
        let expires = time.saturating_add(300);
        let expires_at = chrono::DateTime::from_timestamp(expires as i64, 0)
            .ok_or_else(|| {
                error(
                    AgentErrorCode::InternalError,
                    "Clock outside supported range",
                )
            })?
            .to_rfc3339();
        let offer = GrantOffer {
            id: uuid::Uuid::new_v4().to_string(),
            revision,
            expires_at,
            label: "Selected native actions".into(),
            actions,
        };
        offers.insert(
            offer.id.clone(),
            HeldOffer {
                device: caller.device(),
                offer: offer.clone(),
                expires,
            },
        );
        Ok(vec![offer])
    }
    fn replay(
        &self,
        caller: &VerifiedAgentCaller,
        snapshot: &AgentStateSnapshot,
        invocation: &str,
        request: &[u8],
    ) -> Result<Option<PreparedAgent>, AgentError> {
        let Some(record) = snapshot
            .ledger
            .record_for(caller, invocation)
            .map_err(|e| error(AgentErrorCode::InvocationConflict, e))?
        else {
            return Ok(None);
        };
        if record.request != request {
            return Err(error(
                AgentErrorCode::InvocationConflict,
                "Invocation already binds another canonical intent",
            ));
        }
        let receipt = snapshot
            .ledger
            .mutations
            .get(&record.ids.mutation)
            .ok_or_else(|| error(AgentErrorCode::StorageFailed, "Admission receipt missing"))?;
        Ok(Some(PreparedAgent {
            ids: record.ids.clone(),
            request_digest: receipt.binding.request_digest,
        }))
    }
    pub fn prepare(
        &self,
        caller: &VerifiedAgentCaller,
        invocation: &str,
        input: SpawnInput,
    ) -> Result<PreparedAgent, AgentError> {
        let request = canonical_request(&AgentRequest::Spawn {
            version: 1,
            name: input.name,
            task: input.task,
            grant_set_id: input.grant_set_id.clone(),
        })
        .map_err(|e| error(AgentErrorCode::InvalidRequest, e.to_string()))?;
        let (fresh, snapshot) = self.current(caller)?;
        if let Some(prior) = self.replay(&fresh, &snapshot, invocation, &request)? {
            return Ok(prior);
        }
        let (revision, actions) = self.current_offer(&fresh)?;
        {
            let offers = self
                .offers
                .lock()
                .map_err(|_| error(AgentErrorCode::InternalError, "Offer registry unavailable"))?;
            let held = offers.get(&input.grant_set_id).ok_or_else(|| {
                error(
                    AgentErrorCode::GrantExpired,
                    "Refresh grant offers before a new command",
                )
            })?;
            if held.device != fresh.device() {
                return Err(error(
                    AgentErrorCode::PermissionDenied,
                    "Offer belongs to another caller",
                ));
            }
            if held.expires <= now() {
                return Err(error(AgentErrorCode::GrantExpired, "Grant offer expired"));
            }
            if held.offer.revision != revision || held.offer.actions != actions {
                return Err(error(
                    AgentErrorCode::GrantRevisionChanged,
                    "Grant offer changed",
                ));
            }
        }
        let mut digest = blake3::Hasher::new_derive_key("qOS agent control invocation v1");
        digest.update(&fresh.device());
        digest.update(fresh.room().as_bytes());
        digest.update(invocation.as_bytes());
        let scoped = digest.finalize().to_hex().to_string();
        let ids = AgentIds {
            mutation: format!("prepare:{scoped}"),
            invocation: scoped,
            agent: uuid::Uuid::new_v4().to_string(),
            task: uuid::Uuid::new_v4().to_string(),
            run: uuid::Uuid::new_v4().to_string(),
        };
        let change = AgentChange::new(
            &fresh,
            ids.clone(),
            AgentMutation::Prepare {
                invocation: invocation.into(),
                request: request.clone(),
                grants: actions,
            },
        )
        .map_err(|e| error(AgentErrorCode::InvalidRequest, e))?;
        let commit = OwnerCommitRequest {
            room: fresh.room().into(),
            operation: CommitOperation::Agent {
                change: Box::new(change),
            },
        };
        match futures::executor::block_on(self.owner.commit_agent(commit.clone(), fresh.clone())) {
            Ok(receipt) => {
                receipt
                    .validate_for(&commit)
                    .map_err(|e| error(AgentErrorCode::OutcomeUncertain, e))?;
                Ok(PreparedAgent {
                    ids,
                    request_digest: receipt.binding.request_digest,
                })
            }
            Err(failure) => {
                // A concurrent prepare may have won, or publication may precede a lost barrier.
                // Read-only reconciliation: never resubmit the owner mutation here.
                let (current, snapshot) = self.current(&fresh)?;
                if let Some(prior) = self.replay(&current, &snapshot, invocation, &request)? {
                    return Ok(prior);
                }
                let code = match failure {
                    AgentCommitError::Capacity { .. } => AgentErrorCode::CapacityReached,
                    AgentCommitError::Uncertain { .. } => AgentErrorCode::OutcomeUncertain,
                    AgentCommitError::Conflict { .. } => AgentErrorCode::StorageFailed,
                    AgentCommitError::Rejected(_) => AgentErrorCode::PermissionDenied,
                };
                Err(error(code, failure.to_string()))
            }
        }
    }
    pub fn status(
        &self,
        caller: &VerifiedAgentCaller,
        invocation: &str,
    ) -> Result<AgentOperation, AgentError> {
        let (fresh, snapshot) = self.current(caller)?;
        let r = snapshot
            .ledger
            .record_for(&fresh, invocation)
            .map_err(|e| error(AgentErrorCode::PermissionDenied, e))?
            .ok_or_else(|| {
                error(
                    AgentErrorCode::UnknownInvocation,
                    "No retained admission; this is not permission to replay",
                )
            })?;
        let state = match r.phase {
            AgentControlPhase::Unknown => AgentOperationState::Uncertain,
            AgentControlPhase::Failed => AgentOperationState::Failed,
            _ => AgentOperationState::Accepted,
        };
        Ok(AgentOperation {
            invocation_id: invocation.into(),
            state,
            agent_id: Some(r.ids.agent.clone()),
            task_id: Some(r.ids.task.clone()),
            error: r.error.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{CapOverride, Subject, Verb};
    use qos_agent_control_protocol::{
        DelegationRegistry, GrantScope, GrantVerb, OperationClass, PackagePin, RecordScope,
        ResourceScope,
    };

    fn scope(app: &str, record: RecordScope) -> GrantScope {
        GrantScope {
            app_id: app.into(),
            record,
            verb: GrantVerb::Write,
            classes: vec![OperationClass::Document, OperationClass::View],
            resource: ResourceScope::Any,
            actions: None,
            deny_actions: vec![],
            package: Some(PackagePin {
                package_id: "so.tilo.word-processor".into(),
                version: "1".into(),
                schema_digest: "a".repeat(64),
            }),
            budgets: None,
            explicit_workspace_consent: false,
        }
    }

    #[test]
    fn admission_is_manifest_driven_single_record_and_owner_checked() {
        let mut workspace =
            qos_desktop::Workspace::test_default(qos_desktop::WorkspaceId(3), "Room");
        workspace.set_owner("owner".to_string());
        let room = workspace.room.to_string();
        let owner = workspace.actor_for_person(&"owner".to_string());
        let registry = DelegationRegistry::first_party();
        let writer = qos_agent_control_protocol::WRITER_APP_ID;
        assert!(admissible_scope(
            &workspace,
            &owner,
            &room,
            &registry,
            &scope(writer, RecordScope::Exact("rec-1".into()))
        ));
        // Not in a trusted delegation manifest (no hard-coded app list).
        assert!(!admissible_scope(
            &workspace,
            &owner,
            &room,
            &registry,
            &scope("so.tilo.calculator", RecordScope::Exact("rec-1".into()))
        ));
        assert!(!admissible_scope(
            &workspace,
            &owner,
            &room,
            &DelegationRegistry::empty(),
            &scope(writer, RecordScope::Exact("rec-1".into()))
        ));
        // A workspace wildcard never enters through this admission path.
        let mut wildcard = scope(writer, RecordScope::InWorkspace);
        wildcard.explicit_workspace_consent = true;
        assert!(!admissible_scope(
            &workspace, &owner, &room, &registry, &wildcard
        ));
        // A viewer cannot delegate Write.
        workspace.grant_capability(CapOverride::membership(
            Subject::person("viewer"),
            room.clone(),
            Verb::Read,
        ));
        let viewer = workspace.actor_for_person(&"viewer".to_string());
        assert!(!admissible_scope(
            &workspace,
            &viewer,
            &room,
            &registry,
            &scope(writer, RecordScope::Exact("rec-1".into()))
        ));
    }
}
