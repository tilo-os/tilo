//! Cloud delivery orchestration over the actual held Store turn.
//! Shared plans are inert; only the parent performs the Store barrier/refresh.
use super::{CloudDeliveryError, CloudStoreTurn, DeliveryOperation};
use qos_relay::delivery_v3::{
    auth::VerifiedSession,
    capabilities::STATE_REPLAY_V1,
    commit::{CommitRef, ConsumerId, MAX_CONSUMERS},
    cursor::CursorToken,
    state::ResetReason,
};
use qos_session_peer::ingress::delivery_planning::{
    AckPlan, ConsumerPlan, CursorCut, PlanningFailure, ReplaySelection,
};
use rand::RngCore;

type Result<T> = std::result::Result<T, CloudDeliveryError>;
fn planning(error: PlanningFailure) -> CloudDeliveryError {
    match error {
        PlanningFailure::CurrentDenied | PlanningFailure::UnsupportedPolicy => {
            CloudDeliveryError::Denied
        }
        PlanningFailure::Reset(reason) => CloudDeliveryError::Reset(reason),
        PlanningFailure::Capacity(_) => CloudDeliveryError::Capacity,
        PlanningFailure::Unavailable => CloudDeliveryError::Unavailable,
    }
}

impl CloudStoreTurn {
    fn require_data_operation(
        &self,
        session: &VerifiedSession,
        operation: DeliveryOperation,
    ) -> Result<()> {
        self.require_delivery_operation(operation)?;
        if session.capabilities() & STATE_REPLAY_V1 == 0 {
            return Err(CloudDeliveryError::Denied);
        }
        Ok(())
    }

    pub(crate) fn register_delivery_consumer(
        &mut self,
        session: &VerifiedSession,
    ) -> Result<ConsumerId> {
        self.require_data_operation(session, DeliveryOperation::RegisterConsumer)?;
        let plan = {
            let basis = self.delivery_policy(session)?;
            let ledger = self.delivery_ledger();
            let mut fresh = [0; 16];
            if !ledger
                .consumers
                .contains_key(&session.device_certificate().device_public_key)
            {
                if ledger.consumers.len() >= MAX_CONSUMERS {
                    return Err(CloudDeliveryError::Capacity);
                }
                rand::rngs::OsRng
                    .try_fill_bytes(&mut fresh)
                    .map_err(|_| CloudDeliveryError::Unavailable)?;
            }
            basis.consumer_plan(ledger, fresh).map_err(planning)?
        };
        match plan {
            ConsumerPlan::Existing(id) => Ok(id),
            ConsumerPlan::Upsert(consumer) => {
                let id = consumer.id;
                self.commit_consumer(consumer)?;
                // The parent refreshed the actual turn after its Store barrier.
                // A returned identity cannot stand in for that durable check.
                match self
                    .delivery_policy(session)?
                    .consumer_plan(self.delivery_ledger(), [0; 16])
                    .map_err(planning)?
                {
                    ConsumerPlan::Existing(current) if current == id => Ok(id),
                    ConsumerPlan::Existing(_) => Err(CloudDeliveryError::Denied),
                    ConsumerPlan::Upsert(_) => {
                        Err(CloudDeliveryError::Reset(ResetReason::AuthorizationChanged))
                    }
                }
            }
        }
    }

    pub(crate) fn issue_delivery_cursor(
        &self,
        session: &VerifiedSession,
        committed: CommitRef,
    ) -> Result<CursorToken> {
        // A replay page signs its own boundary cursor inside the same turn.
        if self
            .require_data_operation(session, DeliveryOperation::Replay)
            .is_err()
        {
            self.require_data_operation(session, DeliveryOperation::Cursor)?;
        }
        let issue = self
            .delivery_policy(session)?
            .cursor_issue(self.delivery_ledger(), committed)
            .map_err(planning)?;
        let cursor = self.sign_cursor(issue)?;
        // Intersect disclosure with current policy/time after signing too.
        self.checked_cursor(session, &cursor)?;
        Ok(cursor)
    }

    pub(crate) fn verify_delivery_cursor(
        &self,
        session: &VerifiedSession,
        cursor: &CursorToken,
    ) -> Result<CursorCut> {
        self.require_data_operation(session, DeliveryOperation::Cursor)?;
        self.checked_cursor(session, cursor)
    }

    fn checked_cursor(&self, session: &VerifiedSession, cursor: &CursorToken) -> Result<CursorCut> {
        let basis = self.delivery_policy(session)?;
        let claims = self.authenticate_cursor(cursor)?;
        basis
            .cursor_cut(self.delivery_ledger(), &claims)
            .map_err(planning)
    }

    pub(crate) fn acknowledge_delivery_cursor(
        &mut self,
        session: &VerifiedSession,
        cursor: &CursorToken,
    ) -> Result<()> {
        self.require_data_operation(session, DeliveryOperation::Acknowledge)?;
        let cut = self.checked_cursor(session, cursor)?;
        let plan = self
            .delivery_policy(session)?
            .acknowledge(self.delivery_ledger(), &cut)
            .map_err(planning)?;
        if let AckPlan::Advance(committed) = plan {
            let device = session.device_certificate().device_public_key;
            let consumer = self
                .delivery_ledger()
                .consumers
                .get(&device)
                .ok_or(CloudDeliveryError::Denied)?
                .id;
            self.commit_ack(device, consumer, committed)?;
            let fresh_cut = self.checked_cursor(session, cursor)?;
            if self
                .delivery_policy(session)?
                .acknowledge(self.delivery_ledger(), &fresh_cut)
                .map_err(planning)?
                != AckPlan::Unchanged
            {
                return Err(CloudDeliveryError::Unavailable);
            }
        }
        Ok(())
    }

    pub(crate) fn delivery_replay(
        &self,
        session: &VerifiedSession,
        committed: &CommitRef,
    ) -> Result<ReplaySelection<'_>> {
        self.require_data_operation(session, DeliveryOperation::Replay)?;
        self.delivery_policy(session)?
            .replay(self.delivery_ledger(), committed)
            .map_err(planning)
    }

    /// Borrow one complete actual replay group. The carrier must still preflight
    /// complete frame/byte limits and retain its output credit before encoding.
    pub(crate) fn next_delivery_replay(
        &self,
        session: &VerifiedSession,
        cursor: Option<&CursorToken>,
    ) -> Result<Option<ReplaySelection<'_>>> {
        use std::ops::Bound::{Excluded, Unbounded};
        self.require_data_operation(session, DeliveryOperation::Replay)?;
        let after = cursor
            .map(|cursor| {
                self.checked_cursor(session, cursor)
                    .map(|cut| cut.committed().clone())
            })
            .transpose()?;
        let basis = self.delivery_policy(session)?;
        let ledger = self.delivery_ledger();
        let next = match &after {
            Some(cut) => ledger
                .replay
                .range((Excluded(cut.sequence), Unbounded))
                .next(),
            None => ledger.replay.first_key_value(),
        };
        let Some((sequence, entry)) = next else {
            return Ok(None);
        };
        Self::select_replay(basis, ledger, after.as_ref(), *sequence, entry)
    }

    /// The group after an already selected committed group of this same turn
    /// (multi-group pages); re-applies current policy to that next group.
    pub(crate) fn next_delivery_replay_after(
        &self,
        session: &VerifiedSession,
        after: &CommitRef,
    ) -> Result<Option<ReplaySelection<'_>>> {
        use std::ops::Bound::{Excluded, Unbounded};
        self.require_data_operation(session, DeliveryOperation::Replay)?;
        let basis = self.delivery_policy(session)?;
        let ledger = self.delivery_ledger();
        let Some((sequence, entry)) = ledger
            .replay
            .range((Excluded(after.sequence), Unbounded))
            .next()
        else {
            return Ok(None);
        };
        Self::select_replay(basis, ledger, Some(after), *sequence, entry)
    }

    fn select_replay<'a>(
        basis: qos_session_peer::ingress::delivery_planning::DeliveryPolicyBasis<'_>,
        ledger: &'a qos_relay::delivery_v3::commit::DeliveryLedgerV1,
        after: Option<&CommitRef>,
        sequence: u64,
        entry: &qos_relay::delivery_v3::commit::ReplayCommit,
    ) -> Result<Option<ReplaySelection<'a>>> {
        // Current schema validation requires a complete predecessor chain.
        // Keep this full-cut check if a future retained index permits holes.
        if entry.previous.as_ref() != after {
            return Err(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable));
        }
        let committed = CommitRef {
            sequence,
            digest: entry.digest,
        };
        basis.replay(ledger, &committed).map(Some).map_err(planning)
    }
}

#[cfg(test)]
#[path = "data/tests.rs"]
pub(super) mod tests;
