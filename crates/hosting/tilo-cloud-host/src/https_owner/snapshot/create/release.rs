use super::*;
pub(in crate::https_owner) struct ReleasedSnapshot {
    live: LiveSnapshot,
}
impl LiveSnapshot {
    pub(in crate::https_owner) fn release(
        self,
        turn: &mut CloudStoreTurn,
        session: &VerifiedSession,
    ) -> Result<ReleasedSnapshot> {
        turn.require_delivery_operation(DeliveryOperation::SnapshotRelease)?;
        self.draft.prepared.authorize_identity(turn, session)?;
        let prepared = &self.draft.prepared;
        let expected = turn.snapshot_pin_expectation(
            prepared.selection.pin,
            &prepared.selection.scope,
            prepared.stream.custody(),
        )?;
        require(
            expected.expires_at_ns() == self.draft.expires_at_ns,
            CloudDeliveryError::Denied,
        )?;
        let pair = SnapshotManifestPair::decode(&self.draft.pair.bytes)
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        // Fresh authorization is mandatory; a user's own expired signed pin may
        // still be released, so retained authenticity is the relevant evidence.
        turn.authenticate_snapshot_pair_retained(&pair)?;
        turn.release_snapshot_pin(prepared.selection.pin, &expected)?;
        Ok(ReleasedSnapshot { live: self })
    }
}
impl ReleasedSnapshot {
    /// Successful original owner barrier precedes any physical unlink or quota release.
    pub(in crate::https_owner) fn cleanup(self) -> Result<()> {
        let DraftSnapshot {
            prepared,
            reservation,
            pair,
            ..
        } = self.live.draft;
        drop(prepared);
        drop(pair);
        reservation.remove().map_err(storage)
    }
}
#[cfg(test)]
#[path = "release/tests.rs"]
mod tests;
