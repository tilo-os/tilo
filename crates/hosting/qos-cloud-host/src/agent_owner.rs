//! Private agent admission under the existing Cloud Store lock and durability barrier.
//! No runtime endpoint, launcher, public projection or execution permit is installed here.
use crate::Store;
use qos_action_broker::peer_actions::OwnerPolicyClock;
use qos_peer_state::owner_commit::*;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

mod policy_time;
pub(crate) use policy_time::RuntimePolicyClock;

#[derive(Clone)]
pub struct CloudAgentAuthority {
    policy: Arc<RuntimePolicyClock>,
    root: PathBuf,
    machine: String,
}
impl CloudAgentAuthority {
    pub fn open(root: &Path, machine: &str) -> Result<Self, String> {
        Self::open_with_clock(
            root,
            machine,
            OwnerPolicyClock::new().map_err(|e| e.to_string())?,
        )
    }
    pub(crate) fn open_with_clock(
        root: &Path,
        machine: &str,
        clock: OwnerPolicyClock,
    ) -> Result<Self, String> {
        Self::open_with_policy(root, machine, Arc::new(RuntimePolicyClock::new(clock)))
    }
    pub(crate) fn open_with_policy(
        root: &Path,
        machine: &str,
        policy: Arc<RuntimePolicyClock>,
    ) -> Result<Self, String> {
        // Selected startup must never provision or migrate an ambient replacement
        // before the retained actual Source has checked this very locked Store.
        let store = policy.open_store(root, machine, &policy.capture()?)?;
        canonical_owner(&store.peer().map_err(|e| e.to_string())?)?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        drop(store);
        Ok(Self {
            policy,
            root,
            machine: machine.into(),
        })
    }
    pub(crate) fn policy_context(&self) -> Arc<RuntimePolicyClock> {
        self.policy.clone()
    }
    fn store(&self) -> Result<Store, String> {
        let time = self.policy.capture()?;
        self.policy.open_store(&self.root, &self.machine, &time)
    }
    /// `device` must come from completed authenticated ingress, never request JSON.
    pub fn caller(&self, device: [u8; 32]) -> Result<VerifiedAgentCaller, String> {
        self.store()?.agent_caller(device)
    }
}
impl Store {
    pub(super) fn agent_scope(&self, peer: &qos_peer_state::StatePeer) -> Result<[u8; 32], String> {
        self.ensure_reconciled().map_err(|e| e.to_string())?;
        Ok(owner_scope(
            &self.root.canonicalize().map_err(|e| e.to_string())?,
            &self.state.machine_id,
            peer,
        ))
    }
    pub fn agent_caller(&self, device: [u8; 32]) -> Result<VerifiedAgentCaller, String> {
        let peer = self.peer().map_err(|e| e.to_string())?;
        VerifiedAgentCaller::from_authenticated_source(
            &peer,
            self.agent_scope(&peer)?,
            self.state.generation,
            &self.state.room,
            device,
            self.policy_now().map_err(|e| e.to_string())?,
        )
    }
    pub fn refresh_agent_caller(
        &self,
        caller: &VerifiedAgentCaller,
    ) -> Result<VerifiedAgentCaller, String> {
        let peer = self.peer().map_err(|e| e.to_string())?;
        caller.refresh(
            &peer,
            self.agent_scope(&peer)?,
            self.state.generation,
            self.policy_now().map_err(|e| e.to_string())?,
        )
    }
    /// Explicit host migration selection. No network export or ordinary command calls this.
    /// Qualification must select a matching reader/rollback before runtime activation.
    pub fn activate_private_agent_ledger(
        &mut self,
        caller: &VerifiedAgentCaller,
    ) -> Result<(), AgentCommitError> {
        let peer = self.peer().map_err(store_error)?;
        caller
            .revalidate(
                &peer,
                self.agent_scope(&peer)
                    .map_err(AgentCommitError::Rejected)?,
                self.state.generation,
                self.policy_now().map_err(store_error)?,
            )
            .map_err(AgentCommitError::Rejected)?;
        if self.state.private_schema_version().map_err(store_error)? == crate::AGENT_STATE_VERSION {
            return Ok(());
        }
        if self.state.private_schema_version().map_err(store_error)? != crate::STATE_VERSION
            || self.state.private_agent_ledger.is_some()
        {
            return Err(AgentCommitError::Rejected(
                "invalid private migration source".into(),
            ));
        }
        let mut state = self.state.clone();
        state.select_private_schema().map_err(store_error)?;
        state.private_agent_ledger = Some(PrivateAgentState::V1(PrivateAgentLedger::default()));
        self.commit(state).map_err(store_error)
    }
    pub fn agent_snapshot(
        &self,
        caller: &VerifiedAgentCaller,
    ) -> Result<AgentStateSnapshot, String> {
        let peer = self.peer().map_err(|e| e.to_string())?;
        caller.revalidate(
            &peer,
            self.agent_scope(&peer)?,
            self.state.generation,
            self.policy_now().map_err(|e| e.to_string())?,
        )?;
        let ledger = self
            .state
            .private_agent_ledger
            .as_ref()
            .ok_or("private agent schema has not been selected")?;
        ledger.validate(peer.room(), self.state.generation)?;
        Ok(AgentStateSnapshot {
            room: peer.room().into(),
            generation: self.state.generation,
            governance_revision: governance_revision(&peer)?,
            ledger: ledger.legacy().clone(),
        })
    }
    pub fn commit_agent_request(
        &mut self,
        request: &OwnerCommitRequest,
        caller: &VerifiedAgentCaller,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        self.ensure_reconciled().map_err(store_error)?;
        if self.state.private_schema_version().map_err(store_error)? != crate::AGENT_STATE_VERSION {
            return Err(AgentCommitError::Rejected(
                "explicit private agent schema selection required".into(),
            ));
        }
        let peer = self.peer().map_err(store_error)?;
        let ledger = self
            .state
            .private_agent_ledger
            .as_ref()
            .ok_or_else(|| AgentCommitError::Rejected("private agent ledger missing".into()))?;
        let PrivateAgentState::V1(ledger) = ledger else {
            return Err(AgentCommitError::Rejected(
                "selected v2 preserves legacy admissions read-only".into(),
            ));
        };
        let prepared = prepare_agent_commit(
            &peer,
            ledger,
            self.agent_scope(&peer)
                .map_err(AgentCommitError::Rejected)?,
            self.state.generation,
            request,
            caller,
            self.policy_now().map_err(store_error)?,
        )?;
        let (binding, generation) = match prepared {
            // The current durable aggregate certifies the retained immutable
            // mutation. Its original generation stays in the private ledger.
            PreparedAgentCommit::Replay(prior) => (prior.binding, self.state.generation),
            PreparedAgentCommit::Candidate(candidate) => {
                let binding = request.binding().map_err(AgentCommitError::Rejected)?;
                let generation = self.next_generation().map_err(store_error)?;
                let mut state = self.state.clone();
                state.private_agent_ledger = Some(PrivateAgentState::V1(candidate));
                // Private admission never modifies the peer, publications, grants or document tails.
                self.commit(state).map_err(store_error)?;
                (binding, generation)
            }
        };
        let aggregate_hash = *blake3::hash(&serde_json::to_vec(&self.state).map_err(|e| {
            AgentCommitError::Uncertain {
                generation: Some(generation),
                reason: e.to_string(),
            }
        })?)
        .as_bytes();
        let receipt = OwnerCommitReceipt {
            binding,
            generation,
            aggregate_hash,
        };
        receipt
            .validate_for(request)
            .map_err(|reason| AgentCommitError::Uncertain {
                generation: Some(generation),
                reason,
            })?;
        Ok(receipt)
    }
}
fn store_error(error: anyhow::Error) -> AgentCommitError {
    AgentCommitError::Uncertain {
        generation: error
            .downcast_ref::<crate::StoreCommitFailure>()
            .map(|f| f.generation),
        reason: error.to_string(),
    }
}
impl OwnerCommitService for CloudAgentAuthority {
    fn commit(&self, _request: OwnerCommitRequest) -> CommitFuture {
        Box::pin(async { Err("generic owner commit unavailable on agent adapter".into()) })
    }
    fn health(&self) -> Result<(), String> {
        self.store()?.peer().map(|_| ()).map_err(|e| e.to_string())
    }
    fn commit_agent(
        &self,
        request: OwnerCommitRequest,
        caller: VerifiedAgentCaller,
    ) -> AgentCommitFuture {
        let this = self.clone();
        Box::pin(async move {
            this.store()
                .map_err(|reason| AgentCommitError::Uncertain {
                    generation: None,
                    reason,
                })?
                .commit_agent_request(&request, &caller)
        })
    }
}

impl qos_action_broker::services::WorkspaceAuthority for CloudAgentAuthority {
    fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
        let store = self.store()?;
        let peer = store.peer().map_err(|e| e.to_string())?;
        let owner = canonical_owner(&peer)?;
        let doc = peer.admitted_room().ok_or("admitted room missing")?;
        let mut tree = qos_desktop::WorkspaceTree::default();
        qos_desktop::projection::RoomProjection::apply_admitted_room(
            &mut tree,
            qos_desktop::RoomId::parse(peer.room()).ok_or("invalid room")?,
            doc,
        )?;
        let workspace = tree.workspaces.first_mut().ok_or("workspace missing")?;
        workspace.owner = Some(owner);
        workspace.capabilities = qos_peer_state::room::extract_governed_fields(doc).2;
        // Existing public room projection only; never project private admissions/tasks here.
        Ok(tree)
    }
}
impl qos_agent_control_service::AgentAuthority for CloudAgentAuthority {
    fn current(
        &self,
        caller: &VerifiedAgentCaller,
    ) -> Result<(VerifiedAgentCaller, AgentStateSnapshot), String> {
        let store = self.store()?;
        let fresh = store.refresh_agent_caller(caller)?;
        let snapshot = store.agent_snapshot(&fresh)?;
        Ok((fresh, snapshot))
    }
}
pub struct CloudGrantCatalogue {
    authority: CloudAgentAuthority,
    selected: Vec<qos_agent_control_protocol::GrantAction>,
}
impl CloudAgentAuthority {
    /// Host-selected intent grants only. This creates neither live worker custody nor dispatch authority.
    pub fn grant_catalogue(
        &self,
        selected: Vec<qos_agent_control_protocol::GrantAction>,
    ) -> CloudGrantCatalogue {
        CloudGrantCatalogue {
            authority: self.clone(),
            selected,
        }
    }
}
impl qos_agent_control_service::GrantCatalogue for CloudGrantCatalogue {
    fn current(
        &self,
        caller: &VerifiedAgentCaller,
    ) -> Result<
        qos_agent_control_service::GrantCatalogueSnapshot,
        qos_agent_control_protocol::AgentError,
    > {
        let current = || -> Result<qos_agent_control_service::GrantCatalogueSnapshot, String> {
            let store = self.authority.store()?;
            let fresh = store.refresh_agent_caller(caller)?;
            let peer = store.peer().map_err(|e| e.to_string())?;
            for g in &self.selected {
                let record = if g.record_id == "@qos:package-primary" {
                    ""
                } else {
                    &g.record_id
                };
                if peer.document(&g.app_id, record).is_none() {
                    return Err("selected canonical record unavailable".into());
                }
            }
            let revision = blake3::hash(
                &serde_json::to_vec(&(fresh.governance_revision(), &self.selected))
                    .map_err(|e| e.to_string())?,
            )
            .to_hex()
            .to_string();
            Ok(qos_agent_control_service::GrantCatalogueSnapshot {
                revision,
                actions: self.selected.clone(),
            })
        };
        current().map_err(|message| qos_agent_control_protocol::AgentError {
            code: qos_agent_control_protocol::AgentErrorCode::PermissionDenied,
            message,
        })
    }
}
