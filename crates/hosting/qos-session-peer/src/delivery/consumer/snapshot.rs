//! Source-selected snapshot custody. Stored/wire claims alone never select roots.
use super::*;
use crate::{
    host_bytes::{ByteCredit, HostBytes},
    owner::resources::{OwnedPublicationPin, OwnerDataOperation, OwnerResourceTurn},
    publication_custody::PublicationCustody,
};
use error::DataResult;
use tilo_relay::delivery_v3::state::{SnapshotKind, SnapshotOperation};
pub(crate) struct SnapshotSelection {
    scope: CursorIssue,
    kind: SnapshotKind,
    predecessor: Option<CommitRef>,
    baseline_generation: u64,
    pin: [u8; 16],
    count: usize,
    references: Digest,
    owner_credit: Arc<crate::owner::resources::RetainedSerializedBytes>,
    pool: HostBytes,
    _credit: ByteCredit,
}
impl SnapshotSelection {
    pub(crate) fn scope(&self) -> &CursorIssue {
        &self.scope
    }
    pub(crate) fn kind(&self) -> SnapshotKind {
        self.kind
    }
    pub(crate) fn predecessor(&self) -> Option<&CommitRef> {
        self.predecessor.as_ref()
    }
    pub(crate) fn baseline_generation(&self) -> u64 {
        self.baseline_generation
    }
    pub(crate) fn pin(&self) -> [u8; 16] {
        self.pin
    }
}
fn reference_digest(pin: &PublicationCustody) -> Result<Digest> {
    let mut digest = blake3::Hasher::new();
    digest.update(b"qos-private-snapshot-selection-v1\0");
    digest.update(&(pin.len() as u64).to_le_bytes());
    for i in 0..pin.len() {
        let r = pin.reference(i).context("original pin reference missing")?;
        digest.update(&r.payload);
        digest.update(&[match r.kind {
            Retention::Checkpoint => 0,
            Retention::Delta => 1,
        }]);
    }
    Ok(*digest.finalize().as_bytes())
}
impl SessionOwner {
    pub(crate) fn data_select_snapshot(
        &mut self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        operation: &SnapshotOperation,
        host: &HostBytes,
    ) -> DataResult<(SnapshotSelection, OwnedPublicationPin)> {
        self.with_data_operation(turn, OwnerDataOperation::SnapshotPrepare, |owner| {
            owner.matches_delivery_view(source.view)?;
            let PublicationStorage::V5 {
                roots, delivery, ..
            } = &owner.aggregate.publication
            else {
                bail!("V5 snapshot roots required")
            };
            let (kind, predecessor, baseline_generation, committed, pin) = match operation {
                SnapshotOperation::Baseline => {
                    let (sequence, entry) = delivery
                        .replay
                        .last_key_value()
                        .context("snapshot requires indexed native cut")?;
                    error::require(
                        *sequence >= roots.baseline_generation,
                        DataFailure::Reset(ResetReason::HistoryUnavailable),
                        "baseline is not covered by indexed replay",
                    )?;
                    let count = roots
                        .baseline
                        .len()
                        .checked_add(roots.tail.len())
                        .context("snapshot reference count overflow")?;
                    let pin = owner.pin_snapshot_publications(
                        host,
                        (0..count).map(|i| {
                            if i < roots.baseline.len() {
                                &roots.baseline[i]
                            } else {
                                &roots.tail[i - roots.baseline.len()]
                            }
                        }),
                    )?;
                    (
                        SnapshotKind::Baseline,
                        None,
                        roots.baseline_generation,
                        reference(*sequence, entry),
                        pin,
                    )
                }
                SnapshotOperation::ReplayAfter { cursor } => {
                    let predecessor = owner
                        .verify_delivery_cursor(source, cursor)?
                        .into_cut()
                        .committed()
                        .clone();
                    let (sequence, entry) = delivery
                        .replay
                        .range((
                            std::ops::Bound::Excluded(predecessor.sequence),
                            std::ops::Bound::Unbounded,
                        ))
                        .next()
                        .context("no next committed replay group")?;
                    error::require(
                        entry.previous.as_ref() == Some(&predecessor),
                        DataFailure::Reset(ResetReason::HistoryUnavailable),
                        "snapshot replay predecessor is not retained",
                    )?;
                    let pin = owner.pin_snapshot_publications(host, entry.publications.iter())?;
                    (
                        SnapshotKind::Replay,
                        Some(predecessor),
                        entry.owner_generation,
                        reference(*sequence, entry),
                        pin,
                    )
                }
                SnapshotOperation::Renew { .. } => {
                    return Err(error::failure(
                        DataFailure::InvalidNativeInput,
                        anyhow::anyhow!("renew requires actual retained pin"),
                    ));
                }
            };
            let scope = owner.cursor_issue(source, committed)?;
            let size = std::mem::size_of::<SnapshotSelection>()
                .checked_add(scope.room.capacity())
                .context("snapshot metadata overflow")?;
            let credit = host.try_acquire_bulk(size).ok_or_else(|| {
                error::failure(
                    DataFailure::Capacity(DataCapacity::Bytes),
                    anyhow::anyhow!("snapshot selection metadata budget"),
                )
            })?;
            let selection = SnapshotSelection {
                scope,
                kind,
                predecessor,
                baseline_generation,
                pin: *uuid::Uuid::new_v4().as_bytes(),
                count: pin.len(),
                references: reference_digest(pin.custody())?,
                owner_credit: owner
                    .retained_serialized_credit()
                    .context("snapshot requires original owner custody")?,
                pool: host.clone(),
                _credit: credit,
            };
            Ok((selection, pin))
        })
    }
    /// Re-derive, after a restart, the exact selection a signed manifest this
    /// host issued names: same pin identity, kind, cut and references. Every
    /// claim is checked against the currently retained history and current
    /// consumer scope; the caller must additionally prove the regenerated
    /// stream's chunk digests equal the signed ones before any disclosure.
    pub(crate) fn data_reselect_snapshot(
        &mut self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        issue: &tilo_relay::delivery_v3::state::ManifestIssue,
        host: &HostBytes,
    ) -> DataResult<(SnapshotSelection, OwnedPublicationPin)> {
        self.with_data_operation(turn, OwnerDataOperation::SnapshotPrepare, |owner| {
            owner.matches_delivery_view(source.view)?;
            let history = || DataFailure::Reset(ResetReason::HistoryUnavailable);
            let PublicationStorage::V5 {
                roots, delivery, ..
            } = &owner.aggregate.publication
            else {
                bail!("V5 snapshot roots required")
            };
            let committed = &issue.scope.committed;
            let entry = delivery.replay.get(&committed.sequence);
            error::require(
                entry.is_some_and(|entry| reference(committed.sequence, entry) == *committed),
                history(),
                "signed snapshot cut is no longer retained",
            )?;
            let entry = entry.expect("checked retained entry");
            let count = issue.item_count as usize;
            let pin = match issue.kind {
                SnapshotKind::Baseline => {
                    error::require(
                        issue.predecessor.is_none()
                            && roots.baseline_generation == issue.baseline_generation
                            && roots.baseline.len() + roots.tail.len() >= count,
                        history(),
                        "signed baseline roots are no longer retained",
                    )?;
                    owner.pin_snapshot_publications(
                        host,
                        roots
                            .baseline
                            .iter()
                            .chain(&roots.tail)
                            .take(count)
                            .collect::<Vec<_>>()
                            .into_iter(),
                    )?
                }
                SnapshotKind::Replay => {
                    error::require(
                        entry.previous == issue.predecessor
                            && entry.owner_generation == issue.baseline_generation
                            && entry.publications.len() == count,
                        history(),
                        "signed replay group is no longer retained",
                    )?;
                    owner.pin_snapshot_publications(host, entry.publications.iter())?
                }
            };
            let scope = owner.cursor_issue(source, committed.clone())?;
            error::require(
                scope.store_generation == issue.scope.store_generation,
                DataFailure::Reset(ResetReason::StoreRotated),
                "snapshot store rotated",
            )?;
            error::require(
                scope.authorization_view == issue.scope.authorization_view,
                DataFailure::Reset(ResetReason::AuthorizationChanged),
                "snapshot authorization changed",
            )?;
            error::require(
                scope == issue.scope && issue.pin != [0; 16] && pin.len() == count,
                DataFailure::CurrentDenied,
                "signed snapshot scope differs from the current consumer",
            )?;
            let size = std::mem::size_of::<SnapshotSelection>()
                .checked_add(scope.room.capacity())
                .context("snapshot metadata overflow")?;
            let credit = host.try_acquire_bulk(size).ok_or_else(|| {
                error::failure(
                    DataFailure::Capacity(DataCapacity::Bytes),
                    anyhow::anyhow!("snapshot selection metadata budget"),
                )
            })?;
            let selection = SnapshotSelection {
                scope,
                kind: issue.kind,
                predecessor: issue.predecessor.clone(),
                baseline_generation: issue.baseline_generation,
                pin: issue.pin,
                count: pin.len(),
                references: reference_digest(pin.custody())?,
                owner_credit: owner
                    .retained_serialized_credit()
                    .context("snapshot requires original owner custody")?,
                pool: host.clone(),
                _credit: credit,
            };
            Ok((selection, pin))
        })
    }
    pub(crate) fn data_revalidate_snapshot(
        &self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        selection: &SnapshotSelection,
        pin: &PublicationCustody,
        host: &HostBytes,
    ) -> DataResult<()> {
        self.validate_data_turn(turn, Some(OwnerDataOperation::SnapshotPrepare))?;
        self.revalidate_snapshot(source, selection, pin, host)
            .map_err(|error| self.classify_data_error(error))
    }
    fn revalidate_snapshot(
        &self,
        source: &VerifiedDeliverySource<'_>,
        selection: &SnapshotSelection,
        pin: &PublicationCustody,
        host: &HostBytes,
    ) -> Result<()> {
        self.matches_delivery_view(source.view)?;
        error::require(
            selection.pool.same_pool(host)
                && self
                    .retained_serialized_credit()
                    .is_some_and(|credit| Arc::ptr_eq(&credit, &selection.owner_credit)),
            DataFailure::CurrentDenied,
            "snapshot selection belongs to a different owner resource lifetime",
        )?;
        let ledger = self.delivery_ledger()?;
        let device = source.session.device_certificate();
        let consumer = ledger
            .consumers
            .get(&device.device_public_key)
            .ok_or_else(|| {
                error::failure(
                    DataFailure::CurrentDenied,
                    anyhow::anyhow!("snapshot consumer missing"),
                )
            })?;
        let scope = &selection.scope;
        // Identity precedes reset/index decisions, including asynchronous continuation.
        error::require(
            scope.room == self.peer.room()
                && scope.workspace == self.workspace_id
                && scope.workspace_root == self.peer.authority().user_public_key
                && scope.host_device == self.identity.host_cert.device_public_key
                && scope.host_root == self.identity.host_cert.user_public_key
                && scope.consumer_device == device.device_public_key
                && scope.consumer_root == device.user_public_key
                && scope.consumer == consumer.id
                && consumer.root == device.user_public_key,
            DataFailure::CurrentDenied,
            "snapshot source identity changed",
        )?;
        error::require(
            scope.store_generation == ledger.store_generation,
            DataFailure::Reset(ResetReason::StoreRotated),
            "snapshot store rotated",
        )?;
        error::require(
            scope.authorization_view == source.policy
                && consumer.authorization_view == source.policy,
            DataFailure::Reset(ResetReason::AuthorizationChanged),
            "snapshot authorization changed",
        )?;
        let retained_entry = ledger
            .replay
            .get(&scope.committed.sequence)
            .ok_or_else(|| {
                error::failure(
                    DataFailure::Reset(ResetReason::HistoryUnavailable),
                    anyhow::anyhow!("selected snapshot history is no longer retained"),
                )
            })?;
        error::require(
            &self.cursor_issue(source, scope.committed.clone())? == scope,
            DataFailure::CurrentDenied,
            "snapshot source scope changed",
        )?;
        error::require(
            pin.same_pool(host)
                && pin.len() == selection.count
                && reference_digest(pin)? == selection.references,
            DataFailure::CurrentDenied,
            "snapshot backing does not match original selection",
        )?;
        let PublicationStorage::V5 { roots, .. } = &self.aggregate.publication else {
            unreachable!()
        };
        let retained = match selection.kind {
            SnapshotKind::Baseline => {
                roots.baseline_generation == selection.baseline_generation
                    && roots.baseline.len() + roots.tail.len() >= pin.len()
                    && roots
                        .baseline
                        .iter()
                        .chain(&roots.tail)
                        .take(pin.len())
                        .enumerate()
                        .all(|(i, r)| pin.reference(i) == Some(r))
            }
            SnapshotKind::Replay => {
                retained_entry.previous == selection.predecessor
                    && retained_entry.publications.len() == pin.len()
                    && retained_entry
                        .publications
                        .iter()
                        .enumerate()
                        .all(|(i, r)| pin.reference(i) == Some(r))
            }
        };
        error::require(
            retained,
            DataFailure::Reset(ResetReason::HistoryUnavailable),
            "original snapshot references are no longer retained",
        )
    }
}
impl SessionOwner {
    /// Sign manifest claims for a selection the caller has just revalidated
    /// against Current. The stream must be the one prepared from its pin.
    pub(crate) fn sign_snapshot_manifest(
        &self,
        selection: &SnapshotSelection,
        stream: &crate::delivery_http::snapshot::SnapshotStream,
        expires_at_ns: u64,
        now_ns: u64,
    ) -> Result<tilo_relay::delivery_v3::state::SnapshotManifest> {
        use tilo_relay::delivery_v3::state::{
            ManifestIssue, SNAPSHOT_CHUNK_BYTES, SnapshotManifest,
        };
        ensure!(
            stream.item_count() == selection.count
                && reference_digest(stream.custody())? == selection.references
                && selection.scope.host_device == self.identity.host_cert.device_public_key,
            "snapshot stream differs from its owner selection"
        );
        let issue = ManifestIssue {
            scope: selection.scope.clone(),
            pin: selection.pin,
            kind: selection.kind,
            predecessor: selection.predecessor.clone(),
            baseline_generation: selection.baseline_generation,
            expires_at_ns,
            item_count: u32::try_from(stream.item_count())?,
            stream_bytes: stream.stream_bytes(),
            chunk_size: SNAPSHOT_CHUNK_BYTES as u32,
            chunk_digests: stream.chunk_digests().to_vec(),
        };
        Ok(SnapshotManifest::issue(issue, &self.identity.host, now_ns)?)
    }
}
#[cfg(test)]
#[path = "snapshot/tests.rs"]
mod tests;
