//! Borrowed native witnesses. Metadata and signed cursor claims are never authority.
use super::*;
use crate::committed_authority::CommittedView;
use crate::delivery_planning::{
    AckPlan, ConsumerPlan, CursorCut, DeliveryPolicyBasis, PlanningCapacity, PlanningFailure,
};
use tilo_relay::delivery_v3::{
    auth,
    cursor::{CursorIssue, CursorToken},
    state::ResetReason,
};

pub(crate) struct NativeDeliveryAuthority<'a> {
    pub(crate) view: &'a CommittedView,
    pub(crate) workspace: u64,
    pub(crate) host: &'a DeviceCert,
}
impl auth::Authority for NativeDeliveryAuthority<'_> {
    fn authorize(
        &self,
        binding: &auth::Binding,
        device: &DeviceCert,
        host: &DeviceCert,
        now_ns: u64,
    ) -> std::result::Result<(), auth::AuthError> {
        let peer = self.view.peer();
        let now = SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now_ns);
        if binding.workspace != self.workspace
            || binding.workspace_root != peer.authority().user_public_key
            || host != self.host
            || !host.capabilities.can_act_as_host
            || !peer.certificates().contains(device)
            || !peer.certificates().contains(host)
            || peer.permissions(device.device_public_key, now).is_err()
            || peer.permissions(host.device_public_key, now).is_err()
            || peer.incomplete_reason().is_some()
        {
            return Err(auth::AuthError::PolicyDenied);
        }
        Ok(())
    }
}

/// Only constructed from an actual committed view and a cryptographically authenticated session.
/// The actor still owns descriptor/carrier/boot revalidation before entering this turn.
pub(crate) struct VerifiedDeliverySource<'a> {
    view: &'a CommittedView,
    session: &'a auth::VerifiedSession,
    policy: Digest,
    basis: DeliveryPolicyBasis<'a>,
}
/// Original replay references and payloads borrowed without a metadata allocation.
/// Callers finish page encoding synchronously before releasing the owner borrow.
#[derive(Clone)]
pub(crate) struct BorrowedReplay<'a> {
    pool: &'a PublicationPool,
    publications: std::slice::Iter<'a, PublicationRef>,
}
impl<'a> BorrowedReplay<'a> {
    pub(crate) fn iter(&self) -> Self {
        self.clone()
    }
}
impl<'a> Iterator for BorrowedReplay<'a> {
    type Item = error::DataResult<PublicationView<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        let publication = self.publications.next()?;
        Some(
            self.pool
                .resolve(&publication.payload)
                .map(|bytes| PublicationView {
                    bytes,
                    kind: publication.kind,
                })
                .map_err(|error| error::OwnerDataFailure::new(DataFailure::Unavailable, error)),
        )
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.publications.size_hint()
    }
}
impl ExactSizeIterator for BorrowedReplay<'_> {}

/// Copied validated cut. No borrowed ReplayCommit can cross the owner mutation.
pub(crate) struct ValidatedCursorCut(CursorCut);
pub(crate) struct VerifiedCursor<'a> {
    view: &'a CommittedView,
    cut: ValidatedCursorCut,
}
impl ValidatedCursorCut {
    pub(crate) fn committed(&self) -> &CommitRef {
        self.0.committed()
    }
}
impl VerifiedCursor<'_> {
    pub(crate) fn committed(&self) -> &CommitRef {
        self.cut.committed()
    }
    pub(crate) fn into_cut(self) -> ValidatedCursorCut {
        // Retain the actual native borrow until the copied cut is taken.
        let _ = self.view.generation();
        self.cut
    }
}

fn planning_error(failure: PlanningFailure) -> anyhow::Error {
    let kind = match failure {
        PlanningFailure::CurrentDenied => DataFailure::CurrentDenied,
        PlanningFailure::UnsupportedPolicy => DataFailure::UnsupportedPolicy,
        PlanningFailure::Reset(reason) => DataFailure::Reset(reason),
        PlanningFailure::Capacity(PlanningCapacity::Bytes) => {
            DataFailure::Capacity(DataCapacity::Bytes)
        }
        PlanningFailure::Capacity(PlanningCapacity::Consumers) => {
            DataFailure::Capacity(DataCapacity::Consumer)
        }
        PlanningFailure::Unavailable => DataFailure::Unavailable,
    };
    error::failure(kind, failure)
}
impl SessionOwner {
    fn matches_delivery_view(&self, view: &CommittedView) -> Result<()> {
        self.require_delivery()?;
        self.validate_media_governance_before_commit(&self.peer)
            .map_err(|e| error::failure(DataFailure::CurrentDenied, e))?;
        error::require(
            self.permits_publication()
                && view.generation() == self.aggregate.generation
                && view.peer().room() == self.peer.room()
                && view.peer().authority() == self.peer.authority()
                && view.committed_peer_identity() == self.committed_peer_identity(),
            DataFailure::CurrentDenied,
            "delivery view is not this current committed owner",
        )?;
        Ok(())
    }
    pub(crate) fn delivery_source<'a>(
        &self,
        view: &'a CommittedView,
        session: &'a auth::VerifiedSession,
        now_ns: u64,
    ) -> Result<VerifiedDeliverySource<'a>> {
        self.matches_delivery_view(view)?;
        let authority = NativeDeliveryAuthority {
            view,
            workspace: self.workspace_id,
            host: &self.identity.host_cert,
        };
        let mut revocations = RevocationList::new();
        for entry in view.peer().revocations() {
            revocations.insert(entry)?;
        }
        let capabilities = auth::CapabilityPolicy {
            supported: session.capabilities(),
            allowed: session.capabilities(),
        };
        session
            .revalidate(&auth::VerificationContext {
                binding: session.binding(),
                capabilities,
                credentials: auth::Credentials {
                    device: session.device_certificate(),
                    host: &self.identity.host_cert,
                    device_revocations: &revocations,
                    host_revocations: &revocations,
                },
                authority: &authority,
                now_ns,
            })
            .map_err(|e| error::failure(DataFailure::CurrentDenied, e))?;
        let basis = DeliveryPolicyBasis::inspect_unqualified(
            view.peer(),
            self.workspace_id,
            session.host_certificate(),
            session,
            SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now_ns),
        )
        .map_err(planning_error)?;
        let policy = basis.policy_digest();
        Ok(VerifiedDeliverySource {
            view,
            session,
            policy,
            basis,
        })
    }
    pub(crate) fn register_delivery_consumer(
        &mut self,
        source: &VerifiedDeliverySource<'_>,
    ) -> Result<ConsumerId> {
        self.matches_delivery_view(source.view)?;
        let ledger = self.delivery_ledger()?;
        let device = source.session.device_certificate();
        let fresh_id = if ledger.consumers.contains_key(&device.device_public_key) {
            [0; 16] // Unused for an existing consumer, including a policy refresh.
        } else {
            // Preserve the native preflight before requesting a fresh random ID.
            error::require(
                ledger.consumers.len() < MAX_CONSUMERS,
                DataFailure::Capacity(DataCapacity::Consumer),
                "delivery consumer quota exceeded",
            )?;
            loop {
                let id = *uuid::Uuid::new_v4().as_bytes();
                if !ledger.consumers.values().any(|consumer| consumer.id == id) {
                    break id;
                }
            }
        };
        let consumer = match source
            .basis
            .consumer_plan(ledger, fresh_id)
            .map_err(planning_error)?
        {
            ConsumerPlan::Existing(id) => return Ok(id),
            ConsumerPlan::Upsert(consumer) => consumer,
        };
        let id = consumer.id;
        let mut next = self.aggregate.clone();
        let PublicationStorage::V5 { delivery, .. } = &mut next.publication else {
            unreachable!()
        };
        delivery.consumers.insert(consumer.device, consumer);
        self.commit(next, self.peer.clone())?;
        Ok(id)
    }
    fn cursor_issue(
        &self,
        source: &VerifiedDeliverySource<'_>,
        committed: CommitRef,
    ) -> Result<CursorIssue> {
        self.matches_delivery_view(source.view)?;
        source
            .basis
            .cursor_issue(self.delivery_ledger()?, committed)
            .map_err(planning_error)
    }
    pub(crate) fn issue_delivery_cursor(
        &self,
        source: &VerifiedDeliverySource<'_>,
        committed: CommitRef,
    ) -> Result<CursorToken> {
        Ok(CursorToken::issue(
            self.cursor_issue(source, committed)?,
            &self.identity.host,
        )?)
    }
    pub(crate) fn verify_delivery_cursor<'a>(
        &self,
        source: &VerifiedDeliverySource<'a>,
        token: &CursorToken,
    ) -> Result<VerifiedCursor<'a>> {
        self.matches_delivery_view(source.view)?;
        let signed = token
            .authenticate(&self.identity.host.verifying_key())
            .map_err(|e| error::failure(DataFailure::InvalidNativeInput, e))?;
        let cut = source
            .basis
            .cursor_cut(self.delivery_ledger()?, &signed)
            .map_err(planning_error)?;
        Ok(VerifiedCursor {
            view: source.view,
            cut: ValidatedCursorCut(cut),
        })
    }
    pub(crate) fn acknowledge_delivery(
        &mut self,
        source: &VerifiedDeliverySource<'_>,
        cut: ValidatedCursorCut,
    ) -> Result<()> {
        self.matches_delivery_view(source.view)?;
        let committed = match source
            .basis
            .acknowledge(self.delivery_ledger()?, &cut.0)
            .map_err(planning_error)?
        {
            AckPlan::Unchanged => return Ok(()),
            AckPlan::Advance(committed) => committed,
        };
        // Durable side record first (no aggregate re-encode, no new owner
        // generation); a full or unavailable side log falls back to the
        // aggregate commit, which also folds the log.
        let device = source.session.device_certificate().device_public_key;
        if self.record_ack_side(device, &committed)? {
            return Ok(());
        }
        let mut next = self.aggregate.clone();
        let PublicationStorage::V5 { delivery, .. } = &mut next.publication else {
            unreachable!()
        };
        delivery
            .consumers
            .get_mut(&source.session.device_certificate().device_public_key)
            .context("consumer disappeared")?
            .acknowledged = Some(committed);
        self.commit(next, self.peer.clone())?;
        Ok(())
    }
    pub(in crate::owner) fn delivery_admission_receipt<'a>(
        &'a self,
        source: &VerifiedDeliverySource<'_>,
        delivery: DeliveryId,
        digest: Digest,
    ) -> Result<Option<&'a AdmissionRecord>> {
        self.matches_delivery_view(source.view)?;
        let credit = self
            .retained_serialized_credit()
            .context("receipt lookup requires attached owner custody")?;
        error::require(
            source
                .view
                .retained_serialized_credit()
                .is_some_and(|view| Arc::ptr_eq(view, &credit)),
            DataFailure::Unavailable,
            "receipt source lacks current owner resource custody",
        )?;
        let key = ReceiptKey {
            device: source.session.device_certificate().device_public_key,
            delivery,
        };
        let record = self.delivery_ledger()?.admissions.get(&key);
        if let Some(record) = record {
            error::require(
                record.digest == digest,
                DataFailure::ExactIdConflict,
                "delivery identity names different canonical bytes",
            )?;
        }
        Ok(record)
    }
    pub(in crate::owner) fn validate_staged_delivery_source(
        &self,
        source: &VerifiedDeliverySource<'_>,
        identity: &crate::staging_manifest::StageIdentity,
    ) -> Result<()> {
        self.matches_delivery_view(source.view)?;
        error::require(
            identity.workspace == self.workspace_id
                && identity.device == source.session.device_certificate().device_public_key,
            DataFailure::CurrentDenied,
            "staging identity differs from actual current authenticated owner source",
        )?;
        // This also verifies the exact retained Current custody and rejects an
        // already committed DeliveryId conflict before reading a staged file.
        self.delivery_admission_receipt(source, identity.delivery, identity.digest)?;
        Ok(())
    }
    pub(in crate::owner) fn admit_staged_delivery(
        &mut self,
        source: &VerifiedDeliverySource<'_>,
        item: CompleteStateItem<'_>,
        now: SystemTime,
    ) -> Result<AdmissionOutcome> {
        self.matches_delivery_view(source.view)?;
        self.receive_delivery_with_bound(
            TransportOrigin::RelayDelta(source.session.device_certificate().device_public_key),
            item,
            now,
            64 * 1024 * 1024,
        )
    }
    pub(crate) fn admit_delivery(
        &mut self,
        source: &VerifiedDeliverySource<'_>,
        item: CompleteStateItem<'_>,
        now: SystemTime,
    ) -> Result<AdmissionOutcome> {
        self.matches_delivery_view(source.view)?;
        self.receive_delivery(
            TransportOrigin::RelayDelta(source.session.device_certificate().device_public_key),
            item,
            now,
        )
    }
    pub(crate) fn admit_delivery_group(
        &mut self,
        source: &VerifiedDeliverySource<'_>,
        items: &[CompleteStateItem<'_>],
        now: SystemTime,
    ) -> Result<Vec<AdmissionOutcome>> {
        self.matches_delivery_view(source.view)?;
        self.receive_delivery_group(
            TransportOrigin::RelayDelta(source.session.device_certificate().device_public_key),
            items,
            now,
        )
    }
    pub(in crate::owner) fn borrowed_delivery_replay<'a>(
        &'a self,
        source: &VerifiedDeliverySource<'_>,
        committed: &CommitRef,
    ) -> Result<BorrowedReplay<'a>> {
        self.matches_delivery_view(source.view)?;
        let PublicationStorage::V5 { pool, delivery, .. } = &self.aggregate.publication else {
            unreachable!()
        };
        let selection = source
            .basis
            .replay(delivery, committed)
            .map_err(planning_error)?;
        Ok(BorrowedReplay {
            pool,
            publications: selection.entry().publications.iter(),
        })
    }
    /// Borrow original payloads only while the caller retains this committed view.
    /// HTTP page ownership/byte permits belong to the later route adapter.
    pub(crate) fn replay_delivery<'a>(
        &'a self,
        source: &VerifiedDeliverySource<'_>,
        committed: &CommitRef,
    ) -> Result<Vec<PublicationView<'a>>> {
        self.matches_delivery_view(source.view)?;
        let PublicationStorage::V5 { pool, delivery, .. } = &self.aggregate.publication else {
            unreachable!()
        };
        let selection = source
            .basis
            .replay(delivery, committed)
            .map_err(planning_error)?;
        selection
            .entry()
            .publications
            .iter()
            .map(|publication| {
                Ok(PublicationView {
                    bytes: pool.resolve(&publication.payload)?,
                    kind: publication.kind,
                })
            })
            .collect()
    }
}

#[path = "consumer/snapshot.rs"]
pub(crate) mod snapshot;
