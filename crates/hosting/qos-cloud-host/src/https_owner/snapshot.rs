//! Snapshot lifecycle for the actual held Cloud Store, never a second owner.
//! The parent alone admits resource turns and commits the original Store barrier.
use super::{CloudDeliveryError, CloudStoreSource, CloudStoreTurn, DeliveryOperation};
use qos_relay::delivery_v3::{
    auth::VerifiedSession,
    commit::{CommitRef, Digest},
    cursor::CursorIssue,
    state::{ResetReason, SnapshotKind, SnapshotOperation},
};
use qos_session_peer::ingress::{ByteCredit, HostBytes, PublicationCustody, SnapshotStream};
type Result<T> = std::result::Result<T, CloudDeliveryError>;

/// Created only by actual source-selected roots. No public or wire constructor.
struct Selection {
    source: CloudStoreSource,
    pool: HostBytes,
    scope: CursorIssue,
    kind: SnapshotKind,
    predecessor: Option<CommitRef>,
    baseline_generation: u64,
    pin: [u8; 16],
    references: Digest,
    count: usize,
    _credit: ByteCredit,
}
pub(in crate::https_owner) struct SelectedSnapshot {
    selection: Selection,
    custody: PublicationCustody,
}
pub(in crate::https_owner) struct PreparedSnapshot {
    selection: Selection,
    stream: SnapshotStream,
}
/// Initial selection is synchronous under the parent's actual current turn.
fn planning(
    error: qos_session_peer::ingress::delivery_planning::PlanningFailure,
) -> CloudDeliveryError {
    use qos_session_peer::ingress::delivery_planning::PlanningFailure as P;
    match error {
        P::CurrentDenied | P::UnsupportedPolicy => CloudDeliveryError::Denied,
        P::Reset(reason) => CloudDeliveryError::Reset(reason),
        P::Capacity(_) => CloudDeliveryError::Capacity,
        P::Unavailable => CloudDeliveryError::Unavailable,
    }
}
fn capacity(error: qos_session_peer::ingress::PublicationCustodyFailure) -> CloudDeliveryError {
    match error {
        qos_session_peer::ingress::PublicationCustodyFailure::Capacity => {
            CloudDeliveryError::Capacity
        }
        qos_session_peer::ingress::PublicationCustodyFailure::InvalidPayload => {
            CloudDeliveryError::Unavailable
        }
    }
}
fn history() -> CloudDeliveryError {
    CloudDeliveryError::Reset(ResetReason::HistoryUnavailable)
}
fn require(value: bool, error: CloudDeliveryError) -> Result<()> {
    if value { Ok(()) } else { Err(error) }
}
fn require_capability(session: &VerifiedSession) -> Result<()> {
    use qos_relay::delivery_v3::capabilities::{SNAPSHOT_V1, STATE_REPLAY_V1};
    let required = SNAPSHOT_V1 | STATE_REPLAY_V1;
    require(
        session.capabilities() & required == required,
        CloudDeliveryError::Denied,
    )
}
fn reference_digest(custody: &PublicationCustody) -> Result<Digest> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"qos-cloud-private-snapshot-selection-v1\0");
    hash.update(&(custody.len() as u64).to_le_bytes());
    for i in 0..custody.len() {
        let reference = custody
            .reference(i)
            .ok_or(CloudDeliveryError::Unavailable)?;
        hash.update(&reference.payload);
        hash.update(&[match reference.kind {
            qos_peer_state::transport::Retention::Checkpoint => 0,
            qos_peer_state::transport::Retention::Delta => 1,
        }]);
    }
    Ok(*hash.finalize().as_bytes())
}
pub(in crate::https_owner) fn select(
    source: &CloudStoreSource,
    turn: &CloudStoreTurn,
    session: &VerifiedSession,
    operation: &SnapshotOperation,
) -> Result<SelectedSnapshot> {
    turn.require_delivery_operation(DeliveryOperation::SnapshotSelect)?;
    require(source.owns_turn(turn), CloudDeliveryError::Denied)?;
    require_capability(session)?;
    let pool = turn.host_bytes()?;
    // Reserve escaping scope metadata before planning clones its room string.
    let size = size_of::<Selection>()
        .checked_add(turn.peer().room().len())
        .ok_or(CloudDeliveryError::Capacity)?;
    let credit = pool
        .try_acquire_bulk(size)
        .ok_or(CloudDeliveryError::Capacity)?;
    let policy = turn.delivery_policy(session)?;
    let ledger = turn.delivery_ledger();
    let (kind, predecessor, baseline_generation, committed, custody) = match operation {
        SnapshotOperation::Baseline => {
            let roots = turn.snapshot_roots()?;
            let (sequence, entry) = ledger.replay.last_key_value().ok_or_else(history)?;
            require(*sequence >= roots.baseline_generation, history())?;
            let count = roots
                .baseline
                .len()
                .checked_add(roots.tail.len())
                .ok_or(CloudDeliveryError::Capacity)?;
            let custody = PublicationCustody::from_pool(
                turn.publication_pool(),
                (0..count).map(|i| {
                    if i < roots.baseline.len() {
                        &roots.baseline[i]
                    } else {
                        &roots.tail[i - roots.baseline.len()]
                    }
                }),
                pool,
            )
            .map_err(capacity)?;
            (
                SnapshotKind::Baseline,
                None,
                roots.baseline_generation,
                CommitRef {
                    sequence: *sequence,
                    digest: entry.digest,
                },
                custody,
            )
        }
        SnapshotOperation::ReplayAfter { cursor } => {
            let signed = turn.authenticate_cursor(cursor)?;
            let cut = policy.cursor_cut(ledger, &signed).map_err(planning)?;
            let previous = cut.committed().clone();
            let (sequence, entry) = ledger
                .replay
                .range((
                    std::ops::Bound::Excluded(previous.sequence),
                    std::ops::Bound::Unbounded,
                ))
                .next()
                .ok_or_else(history)?;
            require(entry.previous.as_ref() == Some(&previous), history())?;
            let custody = PublicationCustody::from_pool(
                turn.publication_pool(),
                entry.publications.iter(),
                pool,
            )
            .map_err(capacity)?;
            (
                SnapshotKind::Replay,
                Some(previous),
                entry.owner_generation,
                CommitRef {
                    sequence: *sequence,
                    digest: entry.digest,
                },
                custody,
            )
        }
        SnapshotOperation::Renew { .. } => return Err(CloudDeliveryError::InvalidInput),
    };
    let scope = policy.cursor_issue(ledger, committed).map_err(planning)?;
    require(
        scope.room.capacity() <= turn.peer().room().len(),
        CloudDeliveryError::Capacity,
    )?;
    let selection = Selection {
        source: source.clone(),
        pool: pool.clone(),
        scope,
        kind,
        predecessor,
        baseline_generation,
        pin: *uuid::Uuid::new_v4().as_bytes(),
        references: reference_digest(&custody)?,
        count: custody.len(),
        _credit: credit,
    };
    Ok(SelectedSnapshot { selection, custody })
}
impl SelectedSnapshot {
    /// Worker work owns only original bytes and their credit, not a live turn.
    pub(in crate::https_owner) fn prepare(self) -> Result<PreparedSnapshot> {
        let stream = SnapshotStream::prepare(self.custody, &self.selection.pool).map_err(
            |error| match error {
                qos_session_peer::ingress::SnapshotFailure::Capacity => {
                    CloudDeliveryError::Capacity
                }
                _ => CloudDeliveryError::Unavailable,
            },
        )?;
        Ok(PreparedSnapshot {
            selection: self.selection,
            stream,
        })
    }
}
impl PreparedSnapshot {
    pub(in crate::https_owner) fn revalidate(
        &self,
        turn: &CloudStoreTurn,
        session: &VerifiedSession,
    ) -> Result<()> {
        self.revalidate_for(turn, session, DeliveryOperation::SnapshotSelect)
    }
    fn authorize_identity<'a>(
        &self,
        turn: &'a CloudStoreTurn,
        session: &'a VerifiedSession,
    ) -> Result<qos_session_peer::ingress::delivery_planning::DeliveryPolicyBasis<'a>> {
        authorize_scope(
            &self.selection.source,
            &self.selection.pool,
            &self.selection.scope,
            turn,
            session,
        )
    }
    fn revalidate_for(
        &self,
        turn: &CloudStoreTurn,
        session: &VerifiedSession,
        operation: DeliveryOperation,
    ) -> Result<()> {
        turn.require_delivery_operation(operation)?;
        let policy = self.authorize_identity(turn, session)?;
        let ledger = turn.delivery_ledger();
        let scope = &self.selection.scope;
        let entry = ledger
            .replay
            .get(&scope.committed.sequence)
            .ok_or_else(history)?;
        require(
            &policy
                .cursor_issue(ledger, scope.committed.clone())
                .map_err(planning)?
                == scope,
            CloudDeliveryError::Denied,
        )?;
        let custody = self.stream.custody();
        require(
            custody.same_pool(turn.host_bytes()?)
                && custody.len() == self.selection.count
                && reference_digest(custody)? == self.selection.references,
            CloudDeliveryError::Denied,
        )?;
        let retained = match self.selection.kind {
            SnapshotKind::Baseline => {
                let roots = turn.snapshot_roots()?;
                roots.baseline_generation == self.selection.baseline_generation
                    && roots.baseline.len() + roots.tail.len() >= custody.len()
                    && roots
                        .baseline
                        .iter()
                        .chain(roots.tail)
                        .take(custody.len())
                        .enumerate()
                        .all(|(i, r)| custody.reference(i) == Some(r))
            }
            SnapshotKind::Replay => {
                entry.previous == self.selection.predecessor
                    && entry.publications.len() == custody.len()
                    && entry
                        .publications
                        .iter()
                        .enumerate()
                        .all(|(i, r)| custody.reference(i) == Some(r))
            }
        };
        require(retained, history())
    }
}
fn authorize_scope<'a>(
    source: &CloudStoreSource,
    pool: &HostBytes,
    scope: &CursorIssue,
    turn: &'a CloudStoreTurn,
    session: &'a VerifiedSession,
) -> Result<qos_session_peer::ingress::delivery_planning::DeliveryPolicyBasis<'a>> {
    require(
        source.owns_turn(turn) && pool.same_pool(turn.host_bytes()?),
        CloudDeliveryError::Denied,
    )?;
    require_capability(session)?;
    let policy = turn.delivery_policy(session)?;
    let ledger = turn.delivery_ledger();
    let device = session.device_certificate();
    let host = turn.peer().authority();
    let consumer = ledger
        .consumers
        .get(&device.device_public_key)
        .ok_or(CloudDeliveryError::Denied)?;
    require(
        scope.room == turn.peer().room()
            && scope.workspace == turn.workspace()
            && scope.workspace_root == host.user_public_key
            && scope.host_device == host.device_public_key
            && scope.host_root == host.user_public_key
            && scope.consumer_device == device.device_public_key
            && scope.consumer_root == device.user_public_key
            && scope.consumer == consumer.id
            && consumer.root == device.user_public_key,
        CloudDeliveryError::Denied,
    )?;
    require(
        scope.store_generation == ledger.store_generation,
        CloudDeliveryError::Reset(ResetReason::StoreRotated),
    )?;
    require(
        scope.authorization_view == policy.policy_digest()
            && consumer.authorization_view == policy.policy_digest(),
        CloudDeliveryError::Reset(ResetReason::AuthorizationChanged),
    )?;
    Ok(policy)
}
#[path = "snapshot/create.rs"]
mod create;
#[cfg(test)]
#[path = "snapshot/tests.rs"]
mod tests;

// Opaque phases only; parent routing cannot manufacture selection or commitment.
pub(super) use create::{
    ChunkPrepareRefusal, ChunkReply, CommittedSnapshot, DraftSnapshot, LiveSnapshot,
    PendingSnapshot, PreparedChunk, RecoveryJob, RecoveryPrepared, RecoveryRead, RecoveryReady,
    RecoverySelected, ReleasedSnapshot, Renewal, RenewalCommitted, RenewalDraft, RenewalPending,
    open_recovery,
};
