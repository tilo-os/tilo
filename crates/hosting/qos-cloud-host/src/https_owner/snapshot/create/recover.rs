//! Reconcile only signed candidates selected by the actual current Store pin.
use super::*;
use qos_session_peer::{ingress::RetainedAuthenticatedSnapshotPair, store::SidecarBytes};
pub(in crate::https_owner) struct RecoveryJob {
    source: CloudStoreSource,
    pool: HostBytes,
    pin: [u8; 16],
    reservation: SnapshotReservation,
    credit: ByteCredit,
}
pub(in crate::https_owner) struct RecoveryRead {
    current: Option<SidecarBytes>,
    pending: Option<SidecarBytes>,
    job: RecoveryJob,
}
#[derive(Clone, Copy)]
enum ReconcileAction {
    Keep,
    Finalize,
    DiscardPending,
}
pub(in crate::https_owner) struct RecoverySelected {
    selection: Selection,
    custody: PublicationCustody,
    reservation: SnapshotReservation,
    pair: PairBytes,
    expires_at_ns: u64,
    floor: u64,
    action: ReconcileAction,
}
pub(in crate::https_owner) struct RecoveryPrepared {
    draft: DraftSnapshot,
    floor: u64,
    action: ReconcileAction,
}
pub(in crate::https_owner) struct RecoveryReady {
    prepared: RecoveryPrepared,
}
pub(in crate::https_owner) fn open_recovery(
    source: &CloudStoreSource,
    turn: &CloudStoreTurn,
    session: &VerifiedSession,
    inventory: &HostWorkInventory,
    pin: [u8; 16],
) -> Result<RecoveryJob> {
    turn.require_delivery_operation(DeliveryOperation::SnapshotSelect)?;
    let pool = turn.host_bytes()?;
    require(
        source.owns_turn(turn) && inventory.same_pool(pool),
        CloudDeliveryError::Denied,
    )?;
    require_capability(session)?;
    turn.delivery_policy(session)?;
    // Reserve before opening or decoding either signed member. Sidecar reads
    // separately own their exact backing credit from this same inventory.
    let credit = pool
        .try_acquire_bulk(512 * 1024)
        .ok_or(CloudDeliveryError::Capacity)?;
    let directory = inventory
        .snapshot_directory(turn.directory(), turn.workspace())
        .map_err(storage)?;
    let reservation = directory
        .open_existing(pin)
        .map_err(storage)?
        .ok_or(CloudDeliveryError::Denied)?;
    Ok(RecoveryJob {
        source: source.clone(),
        pool: pool.clone(),
        pin,
        reservation,
        credit,
    })
}
impl RecoveryJob {
    /// Actual bounded file I/O occurs outside the current Store turn.
    pub(in crate::https_owner) fn read(self) -> Result<RecoveryRead> {
        let current = self.reservation.read_current().map_err(storage)?;
        let pending = self.reservation.read_pending().map_err(storage)?;
        Ok(RecoveryRead {
            current,
            pending,
            job: self,
        })
    }
}
fn authenticate(
    turn: &CloudStoreTurn,
    bytes: &Option<SidecarBytes>,
) -> Result<Option<RetainedAuthenticatedSnapshotPair>> {
    bytes
        .as_ref()
        .map(|bytes| {
            let pair = SnapshotManifestPair::decode(bytes.bytes())
                .map_err(|_| CloudDeliveryError::InvalidInput)?;
            turn.authenticate_snapshot_pair_retained(&pair)
        })
        .transpose()
}
impl RecoveryRead {
    pub(in crate::https_owner) fn select(
        mut self,
        turn: &CloudStoreTurn,
        session: &VerifiedSession,
        digest: Digest,
    ) -> Result<RecoverySelected> {
        turn.require_delivery_operation(DeliveryOperation::SnapshotSelect)?;
        require(
            self.job.source.owns_turn(turn) && self.job.pool.same_pool(turn.host_bytes()?),
            CloudDeliveryError::Denied,
        )?;
        require_capability(session)?;
        turn.delivery_policy(session)?;
        let current = authenticate(turn, &self.current)?;
        let pending = match authenticate(turn, &self.pending) {
            Ok(value) => value,
            // An incomplete prepare is inert. Defer its refusal only when a
            // signed canonical candidate exists; selection below must still
            // prove that exact canonical scope/expiry matches the actual live
            // ledger before granting a discard action. A different/uncertain
            // committed pin never authorizes deleting the damaged candidate.
            Err(CloudDeliveryError::InvalidInput) if current.is_some() => None,
            Err(error) => return Err(error),
        };
        let anchor = current
            .as_ref()
            .or(pending.as_ref())
            .ok_or(CloudDeliveryError::Denied)?;
        let scope = &anchor.current().issue().scope;
        authorize_scope(&self.job.source, &self.job.pool, scope, turn, session)?;
        require(
            anchor.current().issue().pin == self.job.pin,
            CloudDeliveryError::Denied,
        )?;
        if let (Some(current), Some(pending)) = (&current, &pending) {
            require(
                current.current().same_content(pending.current()),
                CloudDeliveryError::Denied,
            )?;
            let current_bytes =
                SnapshotManifestPair::decode(self.current.as_ref().unwrap().bytes())
                    .map_err(|_| CloudDeliveryError::InvalidInput)?;
            let pending_bytes =
                SnapshotManifestPair::decode(self.pending.as_ref().unwrap().bytes())
                    .map_err(|_| CloudDeliveryError::InvalidInput)?;
            require(
                pending_bytes.previous() == Some(current_bytes.current()),
                CloudDeliveryError::Denied,
            )?;
        }
        let pin = turn
            .delivery_ledger()
            .pins
            .get(&self.job.pin)
            .ok_or_else(history)?;
        require(
            pin.consumer == scope.consumer
                && pin.authorization_view == scope.authorization_view
                && pin.watermark == scope.committed,
            CloudDeliveryError::Denied,
        )?;
        let matches = |pair: &RetainedAuthenticatedSnapshotPair| {
            pair.current().issue().expires_at_ns == pin.expires_at_ns
        };
        let (chosen, bytes, action) =
            if let Some(pair) = pending.as_ref().filter(|pair| matches(pair)) {
                (
                    pair,
                    self.pending.as_ref().unwrap().bytes(),
                    ReconcileAction::Finalize,
                )
            } else if let Some(pair) = current.as_ref().filter(|pair| matches(pair)) {
                (
                    pair,
                    self.current.as_ref().unwrap().bytes(),
                    if self.pending.is_some() {
                        ReconcileAction::DiscardPending
                    } else {
                        ReconcileAction::Keep
                    },
                )
            } else {
                return Err(CloudDeliveryError::Unavailable);
            };
        let now = turn
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        require(pin.expires_at_ns > now, history())?;
        let chosen_bytes =
            SnapshotManifestPair::decode(bytes).map_err(|_| CloudDeliveryError::InvalidInput)?;
        require(
            *blake3::hash(chosen_bytes.current()).as_bytes() == digest
                || chosen_bytes
                    .previous()
                    .is_some_and(|previous| *blake3::hash(previous).as_bytes() == digest),
            CloudDeliveryError::Denied,
        )?;
        let issue = chosen.current().issue();
        let custody = PublicationCustody::from_pool(
            turn.publication_pool(),
            pin.publications.iter(),
            &self.job.pool,
        )
        .map_err(capacity)?;
        let count = custody.len();
        let references = reference_digest(&custody)?;
        let credit = self
            .job
            .credit
            .split(std::mem::size_of::<Selection>() + issue.scope.room.len())
            .ok_or(CloudDeliveryError::Capacity)?;
        let selection = Selection {
            source: self.job.source,
            pool: self.job.pool,
            scope: issue.scope.clone(),
            kind: issue.kind,
            predecessor: issue.predecessor.clone(),
            baseline_generation: issue.baseline_generation,
            pin: self.job.pin,
            references,
            count,
            _credit: credit,
        };
        require(
            selection.scope.room.capacity() <= selection.scope.room.len(),
            CloudDeliveryError::Capacity,
        )?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(bytes.len())
            .map_err(|_| CloudDeliveryError::Capacity)?;
        require(
            copied.capacity() <= 128 * 1024,
            CloudDeliveryError::Capacity,
        )?;
        copied.extend_from_slice(bytes);
        Ok(RecoverySelected {
            selection,
            custody,
            reservation: self.job.reservation,
            pair: PairBytes {
                bytes: copied,
                _credit: self.job.credit,
            },
            expires_at_ns: pin.expires_at_ns,
            floor: chosen.renewed_through(),
            action,
        })
    }
}
impl RecoverySelected {
    pub(in crate::https_owner) fn prepare(self) -> Result<RecoveryPrepared> {
        let stream = SnapshotStream::prepare(self.custody, &self.selection.pool).map_err(
            |error| match error {
                qos_session_peer::ingress::SnapshotFailure::Capacity => {
                    CloudDeliveryError::Capacity
                }
                _ => CloudDeliveryError::Unavailable,
            },
        )?;
        Ok(RecoveryPrepared {
            draft: DraftSnapshot {
                prepared: PreparedSnapshot {
                    selection: self.selection,
                    stream,
                },
                reservation: self.reservation,
                pair: self.pair,
                expires_at_ns: self.expires_at_ns,
            },
            floor: self.floor,
            action: self.action,
        })
    }
}
impl RecoveryPrepared {
    pub(in crate::https_owner) fn reconcile(
        self,
        turn: &CloudStoreTurn,
        session: &VerifiedSession,
    ) -> Result<RecoveryReady> {
        turn.require_delivery_operation(DeliveryOperation::SnapshotSelect)?;
        let prepared = &self.draft.prepared;
        prepared.authorize_identity(turn, session)?;
        let expected = turn.snapshot_pin_expectation(
            prepared.selection.pin,
            &prepared.selection.scope,
            prepared.stream.custody(),
        )?;
        require(
            expected.expires_at_ns() == self.draft.expires_at_ns,
            CloudDeliveryError::Unavailable,
        )?;
        let pair = SnapshotManifestPair::decode(&self.draft.pair.bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        let authenticated = turn.authenticate_snapshot_pair(&pair)?;
        let issue = authenticated.current().issue();
        require(
            issue.item_count as usize == prepared.stream.item_count()
                && issue.stream_bytes == prepared.stream.stream_bytes()
                && issue.chunk_digests == prepared.stream.chunk_digests(),
            CloudDeliveryError::Denied,
        )?;
        Ok(RecoveryReady { prepared: self })
    }
}
impl RecoveryReady {
    /// The fresh actual ledger selected this action; no unresolved candidate is overwritten.
    pub(in crate::https_owner) fn finish(self) -> Result<LiveSnapshot> {
        match self.prepared.action {
            ReconcileAction::Keep => (),
            ReconcileAction::Finalize => self
                .prepared
                .draft
                .reservation
                .finalize()
                .map_err(storage)?,
            ReconcileAction::DiscardPending => self
                .prepared
                .draft
                .reservation
                .discard_pending()
                .map_err(storage)?,
        }
        Ok(LiveSnapshot {
            draft: self.prepared.draft,
            served_through: self.prepared.floor,
        })
    }
}
#[cfg(test)]
#[path = "recover/tests.rs"]
pub(super) mod tests;
