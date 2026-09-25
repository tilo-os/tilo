//! Inert borrowed policy and ledger planning. No current-owner, resource, or
//! durability qualification is established by these helpers.
pub use crate::owner::PublicationView;
use qos_peer_state::StatePeer;
use tilo_relay::delivery_v3::{
    auth::VerifiedSession,
    commit::*,
    cursor::{CursorIssue, SignedCursorClaims},
    state::ResetReason,
};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanningCapacity {
    Bytes,
    Consumers,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanningFailure {
    CurrentDenied,
    UnsupportedPolicy,
    Reset(ResetReason),
    Capacity(PlanningCapacity),
    Unavailable,
}
impl std::fmt::Display for PlanningFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "delivery planning: {self:?}")
    }
}
impl std::error::Error for PlanningFailure {}
type Result<T> = std::result::Result<T, PlanningFailure>;

pub struct DeliveryPolicyBasis<'a> {
    peer: &'a StatePeer,
    workspace: u64,
    host: &'a DeviceCert,
    session: &'a VerifiedSession,
    policy: Digest,
}
#[derive(Debug, PartialEq, Eq)]
pub enum ConsumerPlan {
    Existing(ConsumerId),
    Upsert(ConsumerState),
}
#[derive(Debug, PartialEq, Eq)]
pub enum AckPlan {
    Unchanged,
    Advance(CommitRef),
}
pub struct CursorCut {
    store: StoreGeneration,
    consumer: ConsumerId,
    device: [u8; 32],
    root: [u8; 32],
    policy: Digest,
    committed: CommitRef,
}
impl CursorCut {
    pub fn committed(&self) -> &CommitRef {
        &self.committed
    }
}
pub struct ReplaySelection<'a> {
    committed: CommitRef,
    entry: &'a ReplayCommit,
}
impl<'a> ReplaySelection<'a> {
    pub fn committed(&self) -> &CommitRef {
        &self.committed
    }
    pub fn entry(&self) -> &'a ReplayCommit {
        self.entry
    }
    pub fn publications<'b>(&'b self, pool: &'b PublicationPool) -> BorrowedPublications<'b> {
        BorrowedPublications {
            pool,
            publications: self.entry.publications.iter(),
        }
    }
}
#[derive(Clone)]
pub struct BorrowedPublications<'a> {
    pool: &'a PublicationPool,
    publications: std::slice::Iter<'a, PublicationRef>,
}
impl<'a> Iterator for BorrowedPublications<'a> {
    type Item = Result<PublicationView<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        let publication = self.publications.next()?;
        Some(
            self.pool
                .resolve(&publication.payload)
                .map(|bytes| PublicationView {
                    bytes,
                    kind: publication.kind,
                })
                .map_err(|_| PlanningFailure::Unavailable),
        )
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.publications.size_hint()
    }
}
impl ExactSizeIterator for BorrowedPublications<'_> {}
impl<'a> DeliveryPolicyBasis<'a> {
    /// Inspect bounded policy bytes only. The caller must first reserve its real
    /// resources and establish current owner/session authorization.
    pub fn inspect_unqualified(
        peer: &'a StatePeer,
        workspace: u64,
        host: &'a DeviceCert,
        session: &'a VerifiedSession,
        now: SystemTime,
    ) -> Result<Self> {
        let policy = policy::inspect(peer, workspace, session.device_certificate(), host, now)
            .map_err(|error| {
                error
                    .downcast_ref::<PlanningFailure>()
                    .copied()
                    .unwrap_or(PlanningFailure::Unavailable)
            })?;
        Ok(Self {
            peer,
            workspace,
            host,
            session,
            policy,
        })
    }
    pub fn policy_digest(&self) -> Digest {
        self.policy
    }
    pub fn consumer_plan(
        &self,
        ledger: &DeliveryLedgerV1,
        fresh_id: ConsumerId,
    ) -> Result<ConsumerPlan> {
        let device = self.session.device_certificate();
        let id = if let Some(existing) = ledger.consumers.get(&device.device_public_key) {
            require(
                existing.root == device.user_public_key,
                PlanningFailure::CurrentDenied,
            )?;
            if existing.authorization_view == self.policy {
                return Ok(ConsumerPlan::Existing(existing.id));
            }
            existing.id
        } else {
            require(
                ledger.consumers.len() < MAX_CONSUMERS,
                PlanningFailure::Capacity(PlanningCapacity::Consumers),
            )?;
            require(
                fresh_id != [0; 16] && !ledger.consumers.values().any(|c| c.id == fresh_id),
                PlanningFailure::Unavailable,
            )?;
            fresh_id
        };
        Ok(ConsumerPlan::Upsert(ConsumerState {
            id,
            device: device.device_public_key,
            root: device.user_public_key,
            authorization_view: self.policy,
            acknowledged: None,
        }))
    }
    fn consumer<'b>(&self, ledger: &'b DeliveryLedgerV1) -> Result<&'b ConsumerState> {
        let device = self.session.device_certificate();
        let consumer = ledger
            .consumers
            .get(&device.device_public_key)
            .ok_or(PlanningFailure::CurrentDenied)?;
        require(
            consumer.root == device.user_public_key,
            PlanningFailure::CurrentDenied,
        )?;
        Ok(consumer)
    }
    pub fn cursor_issue(
        &self,
        ledger: &DeliveryLedgerV1,
        committed: CommitRef,
    ) -> Result<CursorIssue> {
        let consumer = self.consumer(ledger)?;
        require(
            consumer.authorization_view == self.policy,
            PlanningFailure::Reset(ResetReason::AuthorizationChanged),
        )?;
        require(
            committed.sequence <= ledger.resolved_through,
            PlanningFailure::Reset(ResetReason::CursorAhead),
        )?;
        let entry = ledger
            .replay
            .get(&committed.sequence)
            .ok_or(PlanningFailure::Reset(ResetReason::CursorUnknown))?;
        require(
            entry.digest == committed.digest,
            PlanningFailure::Reset(ResetReason::CommitMismatch),
        )?;
        let device = self.session.device_certificate();
        Ok(CursorIssue {
            room: self.peer.room().into(),
            workspace: self.workspace,
            workspace_root: self.peer.authority().user_public_key,
            host_device: self.host.device_public_key,
            host_root: self.host.user_public_key,
            consumer_device: device.device_public_key,
            consumer_root: device.user_public_key,
            consumer: consumer.id,
            store_generation: ledger.store_generation,
            authorization_view: self.policy,
            committed,
        })
    }
    pub fn cursor_cut(
        &self,
        ledger: &DeliveryLedgerV1,
        signed: &SignedCursorClaims,
    ) -> Result<CursorCut> {
        let claims = signed.issue();
        let consumer = self.consumer(ledger)?;
        let device = self.session.device_certificate();
        require(
            claims.room == self.peer.room()
                && claims.workspace == self.workspace
                && claims.workspace_root == self.peer.authority().user_public_key
                && claims.host_device == self.host.device_public_key
                && claims.host_root == self.host.user_public_key
                && claims.consumer_device == device.device_public_key
                && claims.consumer_root == device.user_public_key
                && claims.consumer == consumer.id,
            PlanningFailure::CurrentDenied,
        )?;
        require(
            claims.store_generation == ledger.store_generation,
            PlanningFailure::Reset(ResetReason::StoreRotated),
        )?;
        require(
            claims.authorization_view == self.policy && consumer.authorization_view == self.policy,
            PlanningFailure::Reset(ResetReason::AuthorizationChanged),
        )?;
        let expected = self.cursor_issue(ledger, claims.committed.clone())?;
        require(claims == &expected, PlanningFailure::Unavailable)?;
        Ok(CursorCut {
            store: claims.store_generation,
            consumer: claims.consumer,
            device: claims.consumer_device,
            root: claims.consumer_root,
            policy: claims.authorization_view,
            committed: claims.committed.clone(),
        })
    }
    pub fn acknowledge(&self, ledger: &DeliveryLedgerV1, cut: &CursorCut) -> Result<AckPlan> {
        let consumer = self.consumer(ledger)?;
        let device = self.session.device_certificate();
        require(
            consumer.id == cut.consumer
                && device.device_public_key == cut.device
                && device.user_public_key == cut.root
                && consumer.root == cut.root,
            PlanningFailure::CurrentDenied,
        )?;
        require(
            ledger.store_generation == cut.store,
            PlanningFailure::Reset(ResetReason::StoreRotated),
        )?;
        require(
            self.policy == cut.policy && consumer.authorization_view == cut.policy,
            PlanningFailure::Reset(ResetReason::AuthorizationChanged),
        )?;
        self.cursor_issue(ledger, cut.committed.clone())?;
        if consumer
            .acknowledged
            .as_ref()
            .is_some_and(|ack| ack.sequence >= cut.committed.sequence)
        {
            Ok(AckPlan::Unchanged)
        } else {
            Ok(AckPlan::Advance(cut.committed.clone()))
        }
    }
    pub fn replay<'b>(
        &self,
        ledger: &'b DeliveryLedgerV1,
        committed: &CommitRef,
    ) -> Result<ReplaySelection<'b>> {
        self.cursor_issue(ledger, committed.clone())?;
        let entry = ledger
            .replay
            .get(&committed.sequence)
            .ok_or(PlanningFailure::Unavailable)?;
        Ok(ReplaySelection {
            committed: committed.clone(),
            entry,
        })
    }
}
fn require(condition: bool, failure: PlanningFailure) -> Result<()> {
    if condition { Ok(()) } else { Err(failure) }
}
mod policy;
#[cfg(test)]
mod tests;
