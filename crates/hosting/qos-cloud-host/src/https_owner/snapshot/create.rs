//! Private create phases keep byte/disk custody separate from owner commitment.
use super::*;
use qos_session_peer::{
    ingress::{HostWorkInventory, SnapshotManifestPair},
    store::{HostWorkFailure, SnapshotReservation},
};
struct PairBytes {
    bytes: Vec<u8>,
    _credit: ByteCredit,
}
pub(in crate::https_owner) struct DraftSnapshot {
    prepared: PreparedSnapshot,
    reservation: SnapshotReservation,
    pair: PairBytes,
    expires_at_ns: u64,
}
pub(in crate::https_owner) struct PendingSnapshot {
    draft: DraftSnapshot,
}
pub(in crate::https_owner) struct CommittedSnapshot {
    draft: DraftSnapshot,
}
pub(in crate::https_owner) struct LiveSnapshot {
    draft: DraftSnapshot,
    served_through: u64,
}
impl PreparedSnapshot {
    pub(in crate::https_owner) fn draft(
        self,
        turn: &CloudStoreTurn,
        session: &VerifiedSession,
        inventory: &HostWorkInventory,
    ) -> Result<DraftSnapshot> {
        use qos_relay::delivery_v3::state::{ManifestIssue, SNAPSHOT_CHUNK_BYTES};
        const WORK_BYTES: usize = 512 * 1024;
        let pool = turn.host_bytes()?;
        require(inventory.same_pool(pool), CloudDeliveryError::Denied)?;
        // Includes transient signed/canonical copies and digest vectors; the
        // original stream/backings have independent original-pool custody.
        let credit = pool
            .try_acquire_bulk(WORK_BYTES)
            .ok_or(CloudDeliveryError::Capacity)?;
        self.revalidate_for(turn, session, DeliveryOperation::SnapshotCommit)?;
        let now = turn
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        let expiry = now
            .checked_add(300_000_000_000)
            .ok_or(CloudDeliveryError::Unavailable)?;
        let issue = ManifestIssue {
            scope: self.selection.scope.clone(),
            pin: self.selection.pin,
            kind: self.selection.kind,
            predecessor: self.selection.predecessor.clone(),
            baseline_generation: self.selection.baseline_generation,
            expires_at_ns: expiry,
            item_count: u32::try_from(self.stream.item_count())
                .map_err(|_| CloudDeliveryError::Capacity)?,
            stream_bytes: self.stream.stream_bytes(),
            chunk_size: SNAPSHOT_CHUNK_BYTES as u32,
            chunk_digests: self.stream.chunk_digests().to_vec(),
        };
        let manifest = turn.sign_snapshot_manifest(issue, &self.stream)?;
        let pair = SnapshotManifestPair::new(manifest.as_bytes(), None)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(pair.encoded_len())
            .map_err(|_| CloudDeliveryError::Capacity)?;
        require(bytes.capacity() <= 128 * 1024, CloudDeliveryError::Capacity)?;
        bytes.resize(pair.encoded_len(), 0);
        pair.write_to(&mut bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        drop(manifest);
        let directory = inventory
            .snapshot_directory(turn.directory(), turn.workspace())
            .map_err(storage)?;
        let reservation = directory.reserve(self.selection.pin).map_err(storage)?;
        Ok(DraftSnapshot {
            prepared: self,
            reservation,
            pair: PairBytes {
                bytes,
                _credit: credit,
            },
            expires_at_ns: expiry,
        })
    }
}
impl DraftSnapshot {
    /// Run outside the Store turn. Result retains original worker/file credits.
    pub(in crate::https_owner) fn persist(self) -> Result<PendingSnapshot> {
        self.reservation
            .prepare(&self.pair.bytes)
            .map_err(storage)?;
        Ok(PendingSnapshot { draft: self })
    }
}
impl PendingSnapshot {
    pub(in crate::https_owner) fn commit(
        self,
        turn: &mut CloudStoreTurn,
        session: &VerifiedSession,
    ) -> Result<CommittedSnapshot> {
        let prepared = &self.draft.prepared;
        prepared.revalidate_for(turn, session, DeliveryOperation::SnapshotCommit)?;
        require(
            self.draft.reservation.same_pool(turn.host_bytes()?),
            CloudDeliveryError::Denied,
        )?;
        let pair = SnapshotManifestPair::decode(&self.draft.pair.bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        let authenticated = turn.authenticate_snapshot_pair(&pair)?;
        require(
            authenticated.current().issue().expires_at_ns == self.draft.expires_at_ns,
            CloudDeliveryError::InvalidInput,
        )?;
        // The only constructor follows the successful pending-file barrier.
        // An ambiguous Store result drops live custody but preserves physical
        // pending quota; reconciliation must determine whether the pin committed.
        turn.commit_snapshot_pin(
            prepared.selection.pin,
            &prepared.selection.scope,
            prepared.stream.custody(),
            self.draft.expires_at_ns,
        )?;
        Ok(CommittedSnapshot { draft: self.draft })
    }
}
impl CommittedSnapshot {
    /// Only a successful real owner pin barrier may construct this phase.
    pub(in crate::https_owner) fn finalize(self) -> Result<LiveSnapshot> {
        self.draft.reservation.finalize().map_err(storage)?;
        Ok(LiveSnapshot {
            draft: self.draft,
            served_through: 0,
        })
    }
}
impl LiveSnapshot {
    fn validate_live(
        &self,
        turn: &CloudStoreTurn,
        session: &VerifiedSession,
        operation: DeliveryOperation,
    ) -> Result<crate::https_owner::CloudPinExpectation> {
        turn.require_delivery_operation(operation)?;
        self.draft.prepared.authorize_identity(turn, session)?;
        let prepared = &self.draft.prepared;
        let expected = turn.snapshot_pin_expectation(
            prepared.selection.pin,
            &prepared.selection.scope,
            prepared.stream.custody(),
        )?;
        require(
            expected.expires_at_ns() == self.draft.expires_at_ns,
            history(),
        )?;
        let now = turn
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        require(expected.expires_at_ns() > now, history())?;
        let pair = SnapshotManifestPair::decode(&self.draft.pair.bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        turn.authenticate_snapshot_pair(&pair)?;
        Ok(expected)
    }
    /// Pin identity of this live transfer; inert routing only.
    pub(in crate::https_owner) fn pin(&self) -> [u8; 16] {
        self.draft.prepared.selection.pin
    }
    pub(in crate::https_owner) fn expires_at_ns(&self) -> u64 {
        self.draft.expires_at_ns
    }
    /// Digest of the current signed manifest member, the only chunk target.
    pub(in crate::https_owner) fn current_digest(&self) -> Result<Digest> {
        let pair = SnapshotManifestPair::decode(&self.draft.pair.bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        Ok(*blake3::hash(pair.current()).as_bytes())
    }
    /// The current signed member or its retained previous anchor.
    pub(in crate::https_owner) fn matches_digest(&self, digest: &Digest) -> bool {
        SnapshotManifestPair::decode(&self.draft.pair.bytes).is_ok_and(|pair| {
            *blake3::hash(pair.current()).as_bytes() == *digest
                || pair
                    .previous()
                    .is_some_and(|previous| *blake3::hash(previous).as_bytes() == *digest)
        })
    }
    /// Renewal requires server-observed progress past the last renewal floor.
    pub(in crate::https_owner) fn renewable(&self) -> bool {
        SnapshotManifestPair::decode(&self.draft.pair.bytes)
            .is_ok_and(|pair| self.served_through > pair.renewed_through())
    }
    pub(in crate::https_owner) fn manifest<'a>(
        &'a self,
        turn: &CloudStoreTurn,
        session: &VerifiedSession,
    ) -> Result<&'a [u8]> {
        self.validate_live(turn, session, DeliveryOperation::SnapshotSelect)?;
        let pair = SnapshotManifestPair::decode(&self.draft.pair.bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        Ok(pair.current())
    }
}
fn storage(error: HostWorkFailure) -> CloudDeliveryError {
    match error {
        HostWorkFailure::Capacity => CloudDeliveryError::Capacity,
        HostWorkFailure::Conflict => CloudDeliveryError::Denied,
        HostWorkFailure::Unavailable => CloudDeliveryError::Unavailable,
    }
}
#[path = "create/chunk.rs"]
mod chunk;
#[path = "create/recover.rs"]
mod recover;
#[path = "create/release.rs"]
mod release;
#[path = "create/renew.rs"]
mod renew;
#[cfg(test)]
#[path = "create/tests.rs"]
mod tests;

pub(in crate::https_owner) use chunk::{ChunkPrepareRefusal, ChunkReply, PreparedChunk};
pub(in crate::https_owner) use recover::{
    RecoveryJob, RecoveryPrepared, RecoveryRead, RecoveryReady, RecoverySelected, open_recovery,
};
pub(in crate::https_owner) use release::ReleasedSnapshot;
pub(in crate::https_owner) use renew::{Renewal, RenewalCommitted, RenewalDraft, RenewalPending};
