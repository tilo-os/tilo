use super::*;
pub(in crate::https_owner) enum Renewal {
    Unchanged(LiveSnapshot),
    Pending(RenewalDraft),
}
pub(in crate::https_owner) struct RenewalDraft {
    live: LiveSnapshot,
    next_pair: PairBytes,
    next_expiry: u64,
}
pub(in crate::https_owner) struct RenewalPending {
    draft: RenewalDraft,
}
pub(in crate::https_owner) struct RenewalCommitted {
    draft: RenewalDraft,
}
impl LiveSnapshot {
    pub(in crate::https_owner) fn renew(
        self,
        turn: &CloudStoreTurn,
        session: &VerifiedSession,
        digest: Digest,
    ) -> Result<Renewal> {
        let expected = self.validate_live(turn, session, DeliveryOperation::SnapshotRenew)?;
        let pair = SnapshotManifestPair::decode(&self.draft.pair.bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        if pair
            .previous()
            .is_some_and(|bytes| *blake3::hash(bytes).as_bytes() == digest)
        {
            // Exact retained previous anchor is a lost-reply retry only.
            return Ok(Renewal::Unchanged(self));
        }
        require(
            *blake3::hash(pair.current()).as_bytes() == digest,
            CloudDeliveryError::Denied,
        )?;
        require(
            self.served_through > pair.renewed_through(),
            CloudDeliveryError::Denied,
        )?;
        let credit = self
            .draft
            .prepared
            .selection
            .pool
            .try_acquire_bulk(512 * 1024)
            .ok_or(CloudDeliveryError::Capacity)?;
        let now = turn
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        let next_expiry = now
            .checked_add(300_000_000_000)
            .ok_or(CloudDeliveryError::Unavailable)?;
        require(
            next_expiry > self.draft.expires_at_ns,
            CloudDeliveryError::Denied,
        )?;
        let next = turn.renew_snapshot_manifest(
            &pair,
            &expected,
            &self.draft.prepared.stream,
            next_expiry,
        )?;
        // Combined feasibility includes the QSP2 header before any pending file
        // or owner mutation. Failure preserves the old canonical pin and pair.
        let next_pair = SnapshotManifestPair::new(next.as_bytes(), Some(pair.current()))
            .and_then(|pair| pair.with_renewed_through(self.served_through))
            .map_err(|_| CloudDeliveryError::Capacity)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(next_pair.encoded_len())
            .map_err(|_| CloudDeliveryError::Capacity)?;
        require(bytes.capacity() <= 128 * 1024, CloudDeliveryError::Capacity)?;
        bytes.resize(next_pair.encoded_len(), 0);
        next_pair
            .write_to(&mut bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        Ok(Renewal::Pending(RenewalDraft {
            live: self,
            next_pair: PairBytes {
                bytes,
                _credit: credit,
            },
            next_expiry,
        }))
    }
}
impl RenewalDraft {
    pub(in crate::https_owner) fn persist(self) -> Result<RenewalPending> {
        self.live
            .draft
            .reservation
            .prepare(&self.next_pair.bytes)
            .map_err(storage)?;
        Ok(RenewalPending { draft: self })
    }
}
impl RenewalPending {
    pub(in crate::https_owner) fn commit(
        self,
        turn: &mut CloudStoreTurn,
        session: &VerifiedSession,
    ) -> Result<RenewalCommitted> {
        let expected =
            self.draft
                .live
                .validate_live(turn, session, DeliveryOperation::SnapshotRenew)?;
        let pair = SnapshotManifestPair::decode(&self.draft.next_pair.bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        turn.authenticate_snapshot_pair(&pair)?;
        turn.renew_snapshot_pin(
            self.draft.live.draft.prepared.selection.pin,
            &expected,
            self.draft.next_expiry,
        )?;
        Ok(RenewalCommitted { draft: self.draft })
    }
}
impl RenewalCommitted {
    pub(in crate::https_owner) fn finalize(self) -> Result<LiveSnapshot> {
        self.draft
            .live
            .draft
            .reservation
            .finalize()
            .map_err(storage)?;
        let mut live = self.draft.live;
        live.draft.pair = self.draft.next_pair;
        live.draft.expires_at_ns = self.draft.next_expiry;
        Ok(live)
    }
}
#[cfg(test)]
#[path = "renew/tests.rs"]
mod tests;
