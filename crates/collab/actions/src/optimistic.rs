//! Latency-hiding action lifecycle for a UI backed by a remote model host.
//!
//! The UI always renders a local Automerge replica. Actions apply to that
//! replica immediately and enter this queue; a transport sends the returned
//! envelopes to the authoritative host. Confirmations advance a private
//! confirmed replica in submission order. A rejection or concurrent host
//! update rebuilds the optimistic replica from confirmed state and replays
//! every still-pending action with fresh dependency-correct envelopes.

use std::collections::{HashSet, VecDeque};
use tilo_automerge::ApplyIncoming;

use automerge::AutoCommit;
use automerge::transaction::Transactable;

use crate::{
    Action, ActionEnvelope, ActionError, ActionId, ActionRegistry, EncodedAction, apply_local_by,
    replay,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingStatus {
    Awaiting,
    Accepted,
}

#[derive(Clone, Debug)]
struct PendingAction {
    envelope: ActionEnvelope,
    status: PendingStatus,
}

/// Envelopes invalidated by a rejection or authoritative merge, and their
/// dependency-correct replacements. Send `outbound` in order; late replies
/// naming a `superseded` id are safely ignored by [`OptimisticQueue`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RebaseBatch {
    pub superseded: Vec<ActionId>,
    pub outbound: Vec<ActionEnvelope>,
}

impl RebaseBatch {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.superseded.is_empty() && self.outbound.is_empty()
    }
}

/// Result of an acknowledgement. `Stale` means the id was already replaced
/// during a rebase or was never part of this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acknowledgement {
    Accepted,
    Stale,
}

/// A local optimistic replica plus the confirmed baseline it can rebuild from.
///
/// The transport must treat [`ActionId`] as an idempotency key. Accepted
/// actions are incorporated into the confirmed baseline by
/// [`Self::acknowledge`], so authoritative snapshots passed to
/// [`Self::merge_authoritative`] must not also echo still-pending actions.
/// Buffer such a snapshot until its client action acknowledgements have
/// settled, or send unrelated host changes as a separate Automerge delta.
pub struct OptimisticQueue {
    confirmed: AutoCommit,
    pending: VecDeque<PendingAction>,
    superseded: HashSet<ActionId>,
    optimistic_heads: Vec<automerge::ChangeHash>,
}

impl OptimisticQueue {
    /// Begin at `live`'s current committed state. Pending open Automerge ops
    /// are committed by `get_heads` so the baseline is unambiguous.
    pub fn new(live: &mut AutoCommit) -> Self {
        let optimistic_heads = live.get_heads();
        Self {
            confirmed: live.clone(),
            pending: VecDeque::new(),
            superseded: HashSet::new(),
            optimistic_heads,
        }
    }

    /// Apply now, return the envelope to send, and remember it until the host
    /// accepts or rejects it.
    ///
    /// # Errors
    ///
    /// Returns [`OptimisticError::LiveDiverged`] for out-of-session writes,
    /// or an action/Automerge error when local application fails.
    pub fn submit<A>(
        &mut self,
        live: &mut AutoCommit,
        action: &A,
        encoded: EncodedAction,
    ) -> Result<ActionEnvelope, OptimisticError>
    where
        A: Action,
    {
        self.submit_by(live, action, encoded, None)
    }

    /// [`Self::submit`] with an explicit history actor.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::submit`].
    pub fn submit_by<A>(
        &mut self,
        live: &mut AutoCommit,
        action: &A,
        encoded: EncodedAction,
        actor: Option<&[u8]>,
    ) -> Result<ActionEnvelope, OptimisticError>
    where
        A: Action,
    {
        self.ensure_live_heads(live)?;
        let envelope = apply_local_by(live, action, encoded, actor)?;
        self.optimistic_heads = live.get_heads();
        self.pending.push_back(PendingAction {
            envelope: envelope.clone(),
            status: PendingStatus::Awaiting,
        });
        Ok(envelope)
    }

    /// Mark one reply accepted. Out-of-order replies remain marked until all
    /// predecessors are accepted; the confirmed replica then advances through
    /// the contiguous prefix in order.
    ///
    /// # Errors
    ///
    /// Returns a decode/application error or
    /// [`OptimisticError::ConfirmationBaseMismatch`] without advancing the
    /// confirmed baseline.
    pub fn acknowledge(
        &mut self,
        id: ActionId,
        registry: &ActionRegistry,
    ) -> Result<Acknowledgement, OptimisticError> {
        let Some(pending) = self
            .pending
            .iter_mut()
            .find(|pending| pending.envelope.id == id)
        else {
            return Ok(Acknowledgement::Stale);
        };
        let previous_status = pending.status;
        pending.status = PendingStatus::Accepted;
        let accepted_prefix: Vec<_> = self
            .pending
            .iter()
            .take_while(|pending| pending.status == PendingStatus::Accepted)
            .map(|pending| pending.envelope.clone())
            .collect();
        if accepted_prefix.is_empty() {
            // An earlier action is still awaited: nothing confirms yet.
            return Ok(Acknowledgement::Accepted);
        }
        let mut confirmed = self.confirmed.clone();
        for envelope in &accepted_prefix {
            if let Err(error) = ensure_base(&mut confirmed, envelope) {
                self.restore_status(id, previous_status);
                return Err(error);
            }
            if let Err(error) = replay(&mut confirmed, envelope, registry) {
                self.restore_status(id, previous_status);
                return Err(error.into());
            }
        }
        self.confirmed = confirmed;
        self.pending.drain(..accepted_prefix.len());
        Ok(Acknowledgement::Accepted)
    }

    /// Remove a refused action, roll the live replica back to confirmed state,
    /// and replay every survivor with fresh ids/base heads. The caller sends
    /// the returned replacements and cancels/ignores `superseded` ids.
    pub fn reject(
        &mut self,
        live: &mut AutoCommit,
        id: ActionId,
        registry: &ActionRegistry,
    ) -> Result<RebaseBatch, OptimisticError> {
        let Some(position) = self
            .pending
            .iter()
            .position(|pending| pending.envelope.id == id)
        else {
            return Ok(RebaseBatch::default());
        };
        let Some(removed) = self.pending.remove(position) else {
            return Ok(RebaseBatch::default());
        };
        match self.rebase_from(live, registry, Some(id), None) {
            Ok(batch) => Ok(batch),
            Err(error) => {
                self.pending.insert(position, removed);
                Err(error)
            }
        }
    }

    /// Merge a host replica into the confirmed baseline, then rebase pending
    /// work on top. With no pending work the live replica simply becomes the
    /// merged confirmed state. See the type-level transport contract before
    /// merging a snapshot that contains this client's accepted actions.
    pub fn merge_authoritative(
        &mut self,
        live: &mut AutoCommit,
        authoritative: &mut AutoCommit,
        registry: &ActionRegistry,
    ) -> Result<RebaseBatch, OptimisticError> {
        let mut confirmed = self.confirmed.clone();
        confirmed.merge_incoming(authoritative)?;
        self.rebase_from(live, registry, None, Some(confirmed))
    }

    /// Envelopes to resend after reconnect. No local replay occurs: their
    /// optimistic effects are already present in `live`.
    #[must_use]
    pub fn reconnect_batch(&self) -> Vec<ActionEnvelope> {
        self.pending
            .iter()
            .map(|pending| pending.envelope.clone())
            .collect()
    }

    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.pending.is_empty()
    }

    /// Read-only confirmed state, useful for connection/status UI and tests.
    #[must_use]
    pub fn confirmed(&self) -> &AutoCommit {
        &self.confirmed
    }

    /// The causal heads of the host-confirmed baseline.
    #[must_use]
    pub fn confirmed_heads(&self) -> Vec<automerge::ChangeHash> {
        // The confirmed replica only ever takes whole changes, so its heads
        // are its committed ones: no copy to close a transaction.
        self.confirmed.base_heads()
    }

    #[must_use]
    pub fn is_superseded(&self, id: ActionId) -> bool {
        self.superseded.contains(&id)
    }

    fn ensure_live_heads(&self, live: &mut AutoCommit) -> Result<(), OptimisticError> {
        let actual = live.get_heads();
        if actual == self.optimistic_heads {
            Ok(())
        } else {
            Err(OptimisticError::LiveDiverged)
        }
    }

    fn restore_status(&mut self, id: ActionId, status: PendingStatus) {
        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|pending| pending.envelope.id == id)
        {
            pending.status = status;
        }
    }

    fn rebase_from(
        &mut self,
        live: &mut AutoCommit,
        registry: &ActionRegistry,
        rejected: Option<ActionId>,
        // The new confirmed baseline; `None` keeps the current one.
        confirmed: Option<AutoCommit>,
    ) -> Result<RebaseBatch, OptimisticError> {
        self.ensure_live_heads(live)?;
        let old = self.pending.clone();
        let mut superseded: Vec<_> = old.iter().map(|pending| pending.envelope.id).collect();
        if let Some(rejected) = rejected {
            superseded.push(rejected);
        }

        let mut rebuilt = confirmed.as_ref().unwrap_or(&self.confirmed).clone();
        let mut outbound = Vec::with_capacity(old.len());
        let mut replacement = VecDeque::with_capacity(old.len());
        for pending in old {
            let action = registry.decode(&pending.envelope.action)?;
            let envelope = apply_local_by(
                &mut rebuilt,
                action.as_ref(),
                pending.envelope.action,
                pending.envelope.actor.as_deref(),
            )?;
            outbound.push(envelope.clone());
            replacement.push_back(PendingAction {
                envelope,
                status: PendingStatus::Awaiting,
            });
        }
        if let Some(confirmed) = confirmed {
            self.confirmed = confirmed;
        }
        self.pending = replacement;
        self.superseded.extend(superseded.iter().copied());
        *live = rebuilt;
        self.optimistic_heads = live.get_heads();
        Ok(RebaseBatch {
            superseded,
            outbound,
        })
    }
}

fn ensure_base(
    confirmed: &mut AutoCommit,
    envelope: &ActionEnvelope,
) -> Result<(), OptimisticError> {
    if confirmed.get_heads() == envelope.base_change_hashes() {
        Ok(())
    } else {
        Err(OptimisticError::ConfirmationBaseMismatch(envelope.id))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OptimisticError {
    #[error(transparent)]
    Action(#[from] ActionError),
    #[error(transparent)]
    Automerge(#[from] automerge::AutomergeError),
    #[error("the live replica changed outside the optimistic session")]
    LiveDiverged,
    #[error("confirmed state does not match the recorded base for action {0:?}")]
    ConfirmationBaseMismatch(ActionId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue};
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Serialize, Deserialize)]
    enum Edit {
        Put { key: String, value: i64 },
        Increment { key: String, by: i64 },
    }

    impl Action for Edit {
        fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError> {
            match self {
                Self::Put { key, value } => doc.put(&ROOT, key, ScalarValue::Int(*value))?,
                Self::Increment { key, by } => {
                    let current = doc
                        .get(&ROOT, key)?
                        .and_then(|(value, _)| value.as_i64())
                        .unwrap_or(0);
                    doc.put(&ROOT, key, ScalarValue::Int(current.saturating_add(*by)))?;
                }
            }
            Ok(())
        }
    }

    fn registry() -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        registry.register::<Edit>("edit");
        registry
    }

    fn encoded(edit: &Edit) -> EncodedAction {
        EncodedAction::encode("edit", edit).unwrap()
    }

    fn value(doc: &AutoCommit, key: &str) -> Option<i64> {
        doc.get(&ROOT, key)
            .unwrap()
            .and_then(|(value, _)| value.as_i64())
    }

    /// A submission is visible before the host replies, out-of-order acknowledgements advance in
    /// submission order, reconnect resends without applying twice, and direct live writes are detected
    /// instead of silently lost.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn optimistic_submissions_show_at_once_and_settle_exactly_once() {
        submission_is_visible_before_the_host_replies();
        out_of_order_acknowledgements_advance_in_submission_order();
        reconnect_resends_without_applying_twice();
        direct_live_writes_are_detected_instead_of_silently_lost();
    }

    fn submission_is_visible_before_the_host_replies() {
        let mut live = AutoCommit::new();
        let mut queue = OptimisticQueue::new(&mut live);
        let edit = Edit::Put {
            key: "score".into(),
            value: 7,
        };
        let envelope = queue.submit(&mut live, &edit, encoded(&edit)).unwrap();
        assert_eq!(value(&live, "score"), Some(7));
        assert_eq!(value(queue.confirmed(), "score"), None);
        assert_eq!(queue.reconnect_batch(), [envelope]);
    }

    fn out_of_order_acknowledgements_advance_in_submission_order() {
        let mut live = AutoCommit::new();
        let mut queue = OptimisticQueue::new(&mut live);
        let first = Edit::Put {
            key: "score".into(),
            value: 1,
        };
        let first = queue.submit(&mut live, &first, encoded(&first)).unwrap();
        let second = Edit::Increment {
            key: "score".into(),
            by: 2,
        };
        let second = queue.submit(&mut live, &second, encoded(&second)).unwrap();

        queue.acknowledge(second.id, &registry()).unwrap();
        assert_eq!(queue.pending_len(), 2);
        assert_eq!(value(queue.confirmed(), "score"), None);
        queue.acknowledge(first.id, &registry()).unwrap();
        assert!(queue.is_settled());
        assert_eq!(value(queue.confirmed(), "score"), Some(3));
        assert_eq!(queue.confirmed_heads(), live.get_heads());
    }

    /// A rejection rolls back and rebases survivors, an authoritative merge rebases pending work,
    /// and rejecting a later action keeps and reissues its predecessor.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn host_rejections_and_merges_rebase_the_surviving_pending_work() {
        rejection_rolls_back_and_rebases_survivors();
        authoritative_merge_rebases_pending_work();
        rejecting_a_later_action_keeps_and_reissues_its_predecessor();
    }

    fn rejection_rolls_back_and_rebases_survivors() {
        let mut live = AutoCommit::new();
        let mut queue = OptimisticQueue::new(&mut live);
        let first = Edit::Put {
            key: "score".into(),
            value: 10,
        };
        let first = queue.submit(&mut live, &first, encoded(&first)).unwrap();
        let second = Edit::Increment {
            key: "score".into(),
            by: 2,
        };
        let second = queue.submit(&mut live, &second, encoded(&second)).unwrap();
        assert_eq!(value(&live, "score"), Some(12));

        let batch = queue.reject(&mut live, first.id, &registry()).unwrap();
        assert!(batch.superseded.contains(&first.id));
        assert!(batch.superseded.contains(&second.id));
        assert_eq!(batch.outbound.len(), 1);
        assert_ne!(batch.outbound[0].id, second.id);
        assert_eq!(value(&live, "score"), Some(2));
        assert_eq!(queue.reconnect_batch(), batch.outbound);
        assert_eq!(
            queue.acknowledge(first.id, &registry()).unwrap(),
            Acknowledgement::Stale
        );
    }

    fn authoritative_merge_rebases_pending_work() {
        let mut live = AutoCommit::new();
        live.put(&ROOT, "seed", true).unwrap();
        live.commit();
        let mut remote = AutoCommit::load(&live.clone().save()).unwrap();
        let mut queue = OptimisticQueue::new(&mut live);
        let local = Edit::Increment {
            key: "score".into(),
            by: 2,
        };
        let old = queue.submit(&mut live, &local, encoded(&local)).unwrap();

        remote.put(&ROOT, "remote", 9_i64).unwrap();
        remote.commit();
        let batch = queue
            .merge_authoritative(&mut live, &mut remote, &registry())
            .unwrap();
        assert_eq!(batch.superseded, [old.id]);
        assert_eq!(batch.outbound.len(), 1);
        assert_eq!(value(&live, "remote"), Some(9));
        assert_eq!(value(&live, "score"), Some(2));
        assert_eq!(value(queue.confirmed(), "remote"), Some(9));
        assert_eq!(value(queue.confirmed(), "score"), None);
    }

    fn reconnect_resends_without_applying_twice() {
        let mut live = AutoCommit::new();
        let mut queue = OptimisticQueue::new(&mut live);
        let edit = Edit::Increment {
            key: "score".into(),
            by: 1,
        };
        let sent = queue.submit(&mut live, &edit, encoded(&edit)).unwrap();
        let before = live.get_heads();
        assert_eq!(
            queue.reconnect_batch().as_slice(),
            std::slice::from_ref(&sent)
        );
        assert_eq!(queue.reconnect_batch(), [sent]);
        assert_eq!(live.get_heads(), before);
        assert_eq!(value(&live, "score"), Some(1));
    }

    fn rejecting_a_later_action_keeps_and_reissues_its_predecessor() {
        let mut live = AutoCommit::new();
        let mut queue = OptimisticQueue::new(&mut live);
        let first = Edit::Put {
            key: "score".into(),
            value: 10,
        };
        let first = queue.submit(&mut live, &first, encoded(&first)).unwrap();
        let second = Edit::Increment {
            key: "score".into(),
            by: 2,
        };
        let second = queue.submit(&mut live, &second, encoded(&second)).unwrap();

        let batch = queue.reject(&mut live, second.id, &registry()).unwrap();
        assert_eq!(value(&live, "score"), Some(10));
        assert_eq!(batch.outbound.len(), 1);
        assert_ne!(batch.outbound[0].id, first.id);
        assert!(queue.is_superseded(first.id));
        assert!(queue.is_superseded(second.id));
        assert_eq!(queue.reconnect_batch(), batch.outbound);
    }

    #[test]
    fn rebasing_does_not_discard_edits_made_outside_the_queue() {
        let mut live = AutoCommit::new();
        let mut queue = OptimisticQueue::new(&mut live);
        let edit = Edit::Put {
            key: "score".into(),
            value: 1,
        };
        let sent = queue.submit(&mut live, &edit, encoded(&edit)).unwrap();
        let mut remote = queue.confirmed().clone();
        live.put(&ROOT, "outside", 9_i64).unwrap();
        let heads = live.get_heads();

        assert!(matches!(
            queue.reject(&mut live, sent.id, &registry()),
            Err(OptimisticError::LiveDiverged)
        ));
        assert!(matches!(
            queue.merge_authoritative(&mut live, &mut remote, &registry()),
            Err(OptimisticError::LiveDiverged)
        ));
        assert_eq!(live.get_heads(), heads);
        assert_eq!(value(&live, "outside"), Some(9));
        assert_eq!(queue.reconnect_batch(), [sent]);
    }

    fn direct_live_writes_are_detected_instead_of_silently_lost() {
        let mut live = AutoCommit::new();
        let mut queue = OptimisticQueue::new(&mut live);
        let first = Edit::Put {
            key: "score".into(),
            value: 1,
        };
        queue.submit(&mut live, &first, encoded(&first)).unwrap();

        live.put(&ROOT, "outside", true).unwrap();
        live.commit();
        let second = Edit::Increment {
            key: "score".into(),
            by: 1,
        };
        assert!(matches!(
            queue.submit(&mut live, &second, encoded(&second)),
            Err(OptimisticError::LiveDiverged)
        ));
        assert_eq!(value(&live, "score"), Some(1));
    }
}
