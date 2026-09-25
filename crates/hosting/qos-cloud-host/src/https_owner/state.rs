//! Complete state items admitted inside one reserved Cloud Store turn.
//! The wire receipt is derived only from the record read back after commit.
use super::{CloudDeliveryError, CloudStoreTurn, DeliveryOperation};
use crate::delivery::admission::{StateOutcome, StateRefusal};
use qos_relay::delivery_v3::{
    auth::VerifiedSession,
    capabilities::STATE_REPLAY_V1,
    commit::{AdmissionRecord, DeliveryId, Digest, DurableAdmissionKind},
    state::{AdmissionCode, CommitScope, RetryCode, StateAdmission},
};

type Result<T> = std::result::Result<T, CloudDeliveryError>;

/// Re-encode one durable record; also answers a lost final reply.
pub(crate) fn receipt_admission(
    delivery: DeliveryId,
    digest: Digest,
    record: &AdmissionRecord,
) -> Result<StateAdmission> {
    if record.digest != digest {
        return Err(CloudDeliveryError::Denied);
    }
    let admission = match record.result {
        DurableAdmissionKind::Accepted => StateAdmission::accepted(
            delivery,
            digest,
            CommitScope::NativeCommitted,
            record.operations.clone(),
            record.committed.clone(),
        ),
        DurableAdmissionKind::Duplicate => StateAdmission::duplicate(
            delivery,
            digest,
            CommitScope::NativeCommitted,
            record.operations.clone(),
            record.committed.clone(),
        ),
    };
    admission.map_err(|_| CloudDeliveryError::InvalidInput)
}

impl CloudStoreTurn {
    fn require_state_session(&self, session: &VerifiedSession) -> Result<()> {
        if session.capabilities() & STATE_REPLAY_V1 == 0 {
            return Err(CloudDeliveryError::Denied);
        }
        // Intersect the admitted session with current policy and trusted time.
        self.delivery_policy(session).map(|_| ())
    }

    /// Existing durable receipt for this device/delivery, without admission.
    pub(crate) fn state_receipt(
        &self,
        session: &VerifiedSession,
        delivery: DeliveryId,
        digest: Digest,
    ) -> Result<Option<StateAdmission>> {
        self.require_delivery_operation(DeliveryOperation::ReceiptLookup)?;
        self.require_state_session(session)?;
        let device = session.device_certificate().device_public_key;
        match self.store.delivery_receipt(device, delivery, digest) {
            Ok(Some(record)) => receipt_admission(delivery, digest, &record).map(Some),
            Ok(None) => Ok(None),
            Err(StateRefusal::Conflict) => Ok(Some(StateAdmission::rejected(
                delivery,
                digest,
                AdmissionCode::ConflictingId,
            ))),
            Err(_) => Err(CloudDeliveryError::Unavailable),
        }
    }

    /// Admit one complete canonical item under this reserved turn. The session
    /// device is the only transport origin; the refreshed turn follows commit.
    pub(crate) fn admit_state_item(
        &mut self,
        session: &VerifiedSession,
        delivery: DeliveryId,
        canonical: &[u8],
    ) -> Result<StateAdmission> {
        self.require_delivery_operation(DeliveryOperation::State)?;
        let reserved = self
            .delivery_work
            .as_ref()
            .map(|work| work.input_len)
            .ok_or(CloudDeliveryError::Unavailable)?;
        if canonical.is_empty() || canonical.len() > reserved {
            return Err(CloudDeliveryError::InvalidInput);
        }
        self.require_state_session(session)?;
        let device = session.device_certificate().device_public_key;
        let digest = *blake3::hash(canonical).as_bytes();
        let before = self.store.state.generation;
        let result = self.store.admit_delivery_item(
            device,
            delivery,
            canonical,
            super::transaction::MAX_STAGED_ITEM_BYTES,
        );
        if let Err(StateRefusal::Owner(error)) = &result {
            if let Some(failure) = error.downcast_ref::<crate::StoreCommitFailure>() {
                let failure = *failure;
                self.source.retire();
                return Err(CloudDeliveryError::OwnerUncertain(failure));
            }
            return Err(CloudDeliveryError::Unavailable);
        }
        if self.store.state.generation != before {
            self.refresh_committed()?;
        }
        Ok(match result {
            Ok(StateOutcome::Durable(record)) => receipt_admission(delivery, digest, &record)?,
            Ok(StateOutcome::RetryLater) => {
                StateAdmission::retry_later(delivery, digest, RetryCode::PrerequisiteMissing)
            }
            Ok(StateOutcome::MetadataOnly) | Err(StateRefusal::Unsupported) => {
                StateAdmission::rejected(delivery, digest, AdmissionCode::Unsupported)
            }
            Err(StateRefusal::Invalid) => {
                StateAdmission::rejected(delivery, digest, AdmissionCode::InvalidFrame)
            }
            Err(StateRefusal::Conflict) => {
                StateAdmission::rejected(delivery, digest, AdmissionCode::ConflictingId)
            }
            Err(StateRefusal::Capacity) => {
                StateAdmission::retry_later(delivery, digest, RetryCode::QuotaExceeded)
            }
            Err(StateRefusal::Owner(_)) => unreachable!("handled above"),
        })
    }

    /// Admit a non-empty prefix of `items` under this turn, reserved for their
    /// total length, and return one receipt per admitted item in order.
    /// Consecutive newly accepted document items commit as ONE replay group
    /// (`Store::admit_delivery_group`); otherwise the first item takes the
    /// unchanged single-item path.
    pub(crate) fn admit_state_group(
        &mut self,
        session: &VerifiedSession,
        items: &[(DeliveryId, &[u8])],
    ) -> Result<Vec<StateAdmission>> {
        self.require_delivery_operation(DeliveryOperation::State)?;
        let reserved = self
            .delivery_work
            .as_ref()
            .map(|work| work.input_len)
            .ok_or(CloudDeliveryError::Unavailable)?;
        if items.is_empty() {
            return Err(CloudDeliveryError::InvalidInput);
        }
        // Only the prefix covered by this turn's reservation is admitted here;
        // the caller reserves a later turn for the rest.
        let mut total = 0usize;
        let fitting = items
            .iter()
            .take_while(|(_, canonical)| {
                total = total.saturating_add(canonical.len());
                total <= reserved
            })
            .count();
        if fitting == 0
            || items[..fitting]
                .iter()
                .any(|(_, canonical)| canonical.is_empty())
        {
            return Err(CloudDeliveryError::InvalidInput);
        }
        let items = &items[..fitting];
        self.require_state_session(session)?;
        let device = session.device_certificate().device_public_key;
        let before = self.store.state.generation;
        let result = self.store.admit_delivery_group(
            device,
            items,
            super::transaction::MAX_STAGED_ITEM_BYTES,
        );
        match result {
            Ok(Some(records)) => {
                if self.store.state.generation != before {
                    self.refresh_committed()?;
                }
                items
                    .iter()
                    .zip(&records)
                    .map(|((delivery, canonical), record)| {
                        receipt_admission(*delivery, *blake3::hash(canonical).as_bytes(), record)
                    })
                    .collect()
            }
            Ok(None) => Ok(vec![
                self.admit_state_item(session, items[0].0, items[0].1)?,
            ]),
            Err(StateRefusal::Owner(error)) => {
                if let Some(failure) = error.downcast_ref::<crate::StoreCommitFailure>() {
                    let failure = *failure;
                    self.source.retire();
                    return Err(CloudDeliveryError::OwnerUncertain(failure));
                }
                Err(CloudDeliveryError::Unavailable)
            }
            // Group formation reports no other refusal: each ends the group.
            Err(_) => Err(CloudDeliveryError::Unavailable),
        }
    }

    /// Re-derive the committed peer after this turn's own Store barrier.
    pub(super) fn refresh_committed(&mut self) -> Result<()> {
        self.now_ns = self
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        self.source
            .checked_state(&self.store.state, &self.store.directory, &self.policy_time)
            .map_err(|_| CloudDeliveryError::Denied)?;
        self.peer = crate::delivery::committed_peer(&self.store.state, self.now_ns)
            .map_err(|_| CloudDeliveryError::Denied)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "state/tests.rs"]
mod tests;
