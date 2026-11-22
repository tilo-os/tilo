//! What the workspaces holding the records a window shows, when they are not the window's,
//! say of those records' actions: a person's profile record is held in their own workspace,
//! which decides its changes wherever it is shown.
//!
//! The app holds no other workspace's policy. It names the actions of such records
//! ([`runtime_collab::held_elsewhere::HeldRecord`]); the broker decides each with the actor's
//! ordinary answer in the holding workspace as this host holds it, and decides again on every
//! policy change, sending the decisions when they differ. They say how the window shows its
//! controls; an invocation is admitted on its own against the holding workspace.

use runtime_collab::held_elsewhere::{HeldDecision, HeldRecord, HeldVerdict};
use tokio::sync::mpsc;

use super::wire::Feed;
use super::{AppBrokerState, BrokerActor};

pub(super) use tilo_broker_core::held::NOT_HELD;
/// What a person reads when the holding workspace could not be read just now.
const UNREADABLE: &str = "This can't be checked right now. Try again.";

/// One window's held records, decided again on each policy change.
pub(super) struct HeldSubscription {
    actor: BrokerActor,
    records: Vec<HeldRecord>,
    last: Vec<HeldDecision>,
    tx: mpsc::UnboundedSender<Vec<HeldDecision>>,
}

impl AppBrokerState {
    /// What the workspaces holding `records` say of `actor` running each now.
    pub(super) fn decide_held_elsewhere(
        &self,
        actor: &BrokerActor,
        records: &[HeldRecord],
    ) -> Vec<HeldDecision> {
        let now = self
            .policy_now_ns()
            .map(|ns| ns / 1_000_000_000)
            .inspect_err(|error| {
                tracing::warn!(%error, "held records cannot be decided without the policy clock");
            })
            .ok();
        records
            .iter()
            .map(|record| HeldDecision {
                record: record.clone(),
                verdict: now.map_or_else(
                    || HeldVerdict::Refused(UNREADABLE.into()),
                    |now| self.held_verdict(actor, record, now),
                ),
            })
            .collect()
    }

    fn held_verdict(&self, actor: &BrokerActor, record: &HeldRecord, now: u64) -> HeldVerdict {
        let Some(membership) = self.membership.as_ref() else {
            return HeldVerdict::Refused(NOT_HELD.into());
        };
        membership
            .decide_in_workspace(&record.workspace, |workspace| {
                actor.held_verdict(workspace, record, now)
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, workspace = %record.workspace, "a held record's workspace could not be read");
                HeldVerdict::Refused(UNREADABLE.into())
            })
    }

    /// The workspace holding the resource `resource` the connection `peer` publishes, when its
    /// surface names one other than the window's.
    pub(super) fn held_workspace(&self, peer: u64, resource: &str) -> Option<String> {
        self.surface_tables
            .lock()
            .ok()?
            .get(&peer)?
            .workspace_of(resource)
            .map(str::to_owned)
    }

    /// The decisions of `records` for `actor` now, then each time a policy change makes them
    /// differ, for as long as the feed is held.
    pub(super) fn subscribe_held_elsewhere(
        &self,
        actor: BrokerActor,
        records: Vec<HeldRecord>,
    ) -> Feed<Vec<HeldDecision>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let last = self.decide_held_elsewhere(&actor, &records);
        let _ = tx.send(last.clone());
        let subscription = HeldSubscription {
            actor,
            records,
            last,
            tx,
        };
        match self.held_elsewhere.lock() {
            Ok(mut held) => held.push(subscription),
            Err(poisoned) => poisoned.into_inner().push(subscription),
        }
        Feed::new(rx)
    }

    /// Decide every window's held records again, as a policy change may have changed them, and
    /// send those that differ. A window that let go of its feed is forgotten.
    pub(super) fn refresh_held_elsewhere(&self) {
        let mut held = match self.held_elsewhere.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        held.retain(|subscription| !subscription.tx.is_closed());
        for subscription in held.iter_mut() {
            let now = self.decide_held_elsewhere(&subscription.actor, &subscription.records);
            if now != subscription.last {
                subscription.last.clone_from(&now);
                let _ = subscription.tx.send(now);
            }
        }
    }
}
