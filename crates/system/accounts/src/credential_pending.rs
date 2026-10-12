//! Bounded host proposals reviewed only through the original Passwords owner.
use super::{
    AccessDecision, AccessRequest, BrokerError, CredentialGrant, CredentialOwner, CredentialScope,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const CAPACITY: usize = 8;
const LIFETIME: Duration = Duration::from_secs(120);

/// Private-window review metadata only: no secret value, serialization or
/// public descriptor. Public resources name this proposal by its opaque ID.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialProposal {
    pub id: String,
    pub request: AccessRequest,
    pub scope: CredentialScope,
}
struct Pending {
    proposal: CredentialProposal,
    expires: Instant,
    broker_revision: u64,
    admitted: Arc<dyn Fn() -> bool + Send + Sync>,
}
#[derive(Default)]
pub(super) struct PendingRequests {
    entries: Mutex<BTreeMap<String, Pending>>,
    revision: AtomicU64,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
impl PendingRequests {
    pub(super) fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }
    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::SeqCst);
        let wake = self.wake.lock().ok().and_then(|wake| wake.clone());
        if let Some(wake) = wake {
            wake();
        }
    }
}
impl CredentialOwner {
    /// Bind the original host's frame wake. Never invokes it while holding
    /// the vault, request or wake mutex; registration does no storage I/O.
    pub fn bind_request_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        if let Ok(mut current) = self.requests.wake.lock() {
            *current = Some(wake);
        }
    }
    /// Host-only proposal: this creates no grant and hands out no secret.
    /// `admitted` retains and checks the original native execution, not a
    /// copied boolean. Apps are never lent this owner handle.
    /// # Errors
    /// Refuses foreign scope, invalid origin, unavailable custody, duplicate
    /// proposals or a full bounded queue. Authority runs outside all mutexes.
    pub fn propose_scoped(
        &self,
        mut request: AccessRequest,
        scope: CredentialScope,
        admitted: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<String, BrokerError> {
        if scope.principal != self.principal()
            || scope.device != self.device()
            || !scope.valid()
            || !admitted()
        {
            return Err(BrokerError::Denied);
        }
        super::validate_app_id(&request.app_id)?;
        super::validate_purpose(&request.purpose)?;
        let origin =
            super::canonical_origin(request.origin.as_deref().ok_or(BrokerError::Denied)?)?;
        request.origin = Some(origin.clone());
        self.proposals()?;
        let broker = self.broker()?;
        let current = broker.lock().map_err(|_| BrokerError::Denied)?;
        let item = current
            .store
            .get(&request.item_id)
            .ok_or(BrokerError::NotFound)?;
        if !item.origins.contains(&origin) {
            return Err(BrokerError::OriginMismatch);
        }
        if current
            .current_grants()
            .iter()
            .any(|grant| exact(grant, &request, &scope))
        {
            return Err(BrokerError::Denied);
        }
        let broker_revision = current.revision;
        drop(current);
        drop(item);
        if !admitted() {
            return Err(BrokerError::Denied);
        }
        let now = Instant::now();
        let mut entries = self
            .requests
            .entries
            .lock()
            .map_err(|_| BrokerError::Denied)?;
        if entries.len() >= CAPACITY
            || entries.values().any(|pending| {
                pending.proposal.request == request && pending.proposal.scope == scope
            })
        {
            return Err(BrokerError::Denied);
        }
        let id = uuid::Uuid::new_v4().to_string();
        entries.insert(
            id.clone(),
            Pending {
                proposal: CredentialProposal {
                    id: id.clone(),
                    request,
                    scope,
                },
                expires: now.checked_add(LIFETIME).ok_or(BrokerError::Denied)?,
                broker_revision,
                admitted,
            },
        );
        drop(entries);
        self.requests.changed();
        Ok(id)
    }
    /// Review the current original-owner proposals. Stale authority, expiry
    /// and vault changes remove them; no background decision creates access.
    /// # Errors
    /// Refuses poisoned original custody. Checks never run under its mutexes.
    pub fn proposals(&self) -> Result<Vec<CredentialProposal>, BrokerError> {
        let broker = self.broker()?;
        let revision = broker.lock().map_err(|_| BrokerError::Denied)?.revision;
        let snapshot = {
            let entries = self
                .requests
                .entries
                .lock()
                .map_err(|_| BrokerError::Denied)?;
            entries
                .values()
                .map(|pending| {
                    (
                        pending.proposal.clone(),
                        pending.expires,
                        pending.broker_revision,
                        pending.admitted.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let now = Instant::now();
        let mut live = Vec::new();
        let mut retired = Vec::new();
        for (proposal, expires, admitted_revision, admitted) in snapshot {
            if expires > now && admitted_revision == revision && admitted() {
                live.push(proposal);
            } else {
                retired.push(proposal.id);
            }
        }
        let changed = {
            let mut entries = self
                .requests
                .entries
                .lock()
                .map_err(|_| BrokerError::Denied)?;
            retired
                .into_iter()
                .filter(|id| entries.remove(id).is_some())
                .count()
                > 0
        };
        if changed {
            self.requests.changed();
        }
        Ok(live)
    }
    /// Time until the protected window must retire a pending presentation.
    /// It uses the shared host wake scheduler, never polling or extending TTL.
    pub fn proposal_wake_after(&self) -> Option<Duration> {
        let entries = self.requests.entries.lock().ok()?;
        entries
            .values()
            .map(|pending| pending.expires.saturating_duration_since(Instant::now()))
            .min()
    }
    /// Original Passwords confirmation door. Consumes the opaque request once
    /// and returns permission metadata only. A one-shot credential read cannot
    /// authorize persistent provider startup and is deliberately refused here.
    /// # Errors
    /// Refuses replay, expiry, changed custody or retired admission. Failed
    /// post-decision authority revokes only the new exact permission ID.
    pub fn decide_proposal(
        &self,
        id: &str,
        decision: AccessDecision,
    ) -> Result<Option<CredentialGrant>, BrokerError> {
        if decision == AccessDecision::AllowOnce {
            return Err(BrokerError::Denied);
        }
        let pending = self
            .requests
            .entries
            .lock()
            .map_err(|_| BrokerError::Denied)?
            .remove(id)
            .ok_or(BrokerError::Denied)?;
        self.requests.changed();
        if decision == AccessDecision::Deny {
            return Ok(None);
        }
        if pending.expires <= Instant::now() || !(pending.admitted)() {
            return Err(BrokerError::Denied);
        }
        let proposal = pending.proposal;
        let broker = self.broker()?;
        let mut current = broker.lock().map_err(|_| BrokerError::Denied)?;
        if current.revision != pending.broker_revision {
            return Err(BrokerError::Denied);
        }
        let before = current.current_grants();
        if before
            .iter()
            .any(|grant| exact(grant, &proposal.request, &proposal.scope))
        {
            return Err(BrokerError::Denied);
        }
        drop(current.request_scoped(&proposal.request, &proposal.scope, |_, _, _| decision)?);
        let grant = current
            .current_grants()
            .into_iter()
            .find(|grant| exact(grant, &proposal.request, &proposal.scope))
            .ok_or(BrokerError::Denied)?;
        drop(current);
        if !(pending.admitted)() {
            let mut current = broker.lock().map_err(|_| BrokerError::Denied)?;
            current.revoke_grant(&grant)?;
            drop(current);
            self.requests.changed();
            return Err(BrokerError::Denied);
        }
        self.requests.changed();
        Ok(Some(grant))
    }
}
fn exact(grant: &CredentialGrant, request: &AccessRequest, scope: &CredentialScope) -> bool {
    grant.app_id == request.app_id
        && grant.item_id == request.item_id
        && grant.origin == request.origin
        && grant.scope.as_ref() == Some(scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_original_proposal_retires_its_presentation_and_never_grants_access() {
        let (root, broker, request, scope) = super::super::owner_tests::fixture();
        let broker = Arc::new(Mutex::new(broker));
        let owner =
            CredentialOwner::from_broker(&scope.principal, &scope.device, broker.clone()).unwrap();
        let id = owner
            .propose_scoped(request, scope, Arc::new(|| true))
            .unwrap();
        let before = owner.revision().unwrap();
        owner
            .requests
            .entries
            .lock()
            .unwrap()
            .get_mut(&id)
            .unwrap()
            .expires = Instant::now();
        assert!(owner.proposal_wake_after().unwrap().is_zero());
        assert!(owner.proposals().unwrap().is_empty());
        assert!(owner.revision().unwrap() > before);
        assert!(owner.proposal_wake_after().is_none());
        assert!(
            owner
                .decide_proposal(&id, AccessDecision::AllowAlways)
                .is_err()
        );
        assert!(owner.grants().unwrap().is_empty());
        drop(owner);
        drop(broker);
        std::fs::remove_dir_all(root).unwrap();
    }
}
