//! Framework-owned action dispatch every app gets by default.
//!
//! Adopting the qOS [action model](qos_actions) used to mean each app
//! copy-pasting the *same* four pieces of plumbing: a slot to bracket the undo
//! boundary before the first action of an event, the encode-and-dispatch call
//! itself, a record of the last action so a test can prove an affordance and its
//! Ctrl+K [`ActionSpec`](qos_actions::ActionSpec) build the identical action, and
//! a no-op transport for the on-screen affordance path. That boilerplate is
//! hoisted here so an app declares only its own domain — its `Action` enum and
//! resource tree — and embeds one [`ActionHost`] field.
//!
//! ```ignore
//! struct MyApp { actions: ui::ActionHost, /* … */ }
//!
//! // At every affordance that mutates shared state:
//! self.actions.dispatch(doc, MY_ACTION_TYPE, &action);
//!
//! impl Widget for MyApp {
//!     fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
//!         self.actions.take_committed()
//!     }
//! }
//! ```

use automerge::{AutoCommit, ChangeHash};
use qos_actions::{Action, EncodedAction};

/// Framework-owned action dispatch an app embeds as a single field, replacing the
/// hand-rolled `perform_action` method, the per-app `DiscardTransport`, and the
/// `action_commit_before` / `last_dispatched` fields every migrated app used to
/// copy-paste verbatim.
///
/// [`dispatch`](Self::dispatch) is `perform_action`, once, for everyone: it
/// brackets the undo boundary, encodes the action, remembers it, and dispatches
/// it through the toolkit's default local transport (applied via the executor so
/// it replays byte-identically on peers). The host ([`crate::app::App`]) drains
/// [`take_committed`](Self::take_committed) after each event — wired through
/// [`Widget::take_committed_action`](crate::widget::Widget::take_committed_action)
/// — and records exactly one undo boundary bracketing the event, the same
/// `record_committed_action` path a Ctrl+K palette dispatch takes.
///
/// That boundary carries no author: what crosses this seam is heads, not
/// identity. It is reachable by the interactive Ctrl+Z only because an
/// author-scoped undo claims unattributed entries as well as its own — see
/// `UndoLog::claimable_by`.
#[derive(Default)]
pub struct ActionHost {
    /// The document heads captured just before the first action of the current
    /// event was dispatched. The host drains this via
    /// [`Widget::take_committed_action`](crate::widget::Widget::take_committed_action)
    /// to record exactly one undo boundary bracketing the action(s).
    commit_before: Option<Vec<ChangeHash>>,
    /// The last [`EncodedAction`] dispatched. Lets a test prove an affordance and
    /// the Ctrl+K [`ActionSpec`](qos_actions::ActionSpec) build the byte-identical
    /// action — one source of truth, not two code paths.
    last_dispatched: Option<EncodedAction>,
}

impl ActionHost {
    /// A fresh host with nothing bracketed or dispatched yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Dispatch one domain `action` against `doc` under `type_id`. This IS
    /// `perform_action`, once, for every app:
    ///
    /// * **Brackets the undo boundary** — captures `doc.get_heads()` into
    ///   `commit_before` on the *first* action of the event only. `get_heads`
    ///   flushes any pending app writes, so the bracket covers only the action's
    ///   own change; the host reads it back via [`take_committed`](Self::take_committed).
    /// * **Encodes** the action under `type_id` and remembers it as
    ///   [`last_dispatched`](Self::last_dispatched).
    /// * **Dispatches** it through the toolkit's default
    ///   [`LocalTransport`](qos_actions::LocalTransport) (applied via the executor
    ///   with a pinned actor + committed change, so it replays byte-identically on
    ///   peers). Peers receive the resulting change through ordinary CRDT sync, so
    ///   this local affordance path has nothing extra to replicate.
    ///
    /// A dispatch error is logged at WARN and otherwise swallowed, exactly as the
    /// per-app `perform_action` did — an affordance never panics the app.
    pub fn dispatch<A>(&mut self, doc: &mut AutoCommit, type_id: &str, action: &A)
    where
        A: Action + serde::Serialize,
    {
        if self.commit_before.is_none() {
            self.commit_before = Some(doc.get_heads());
        }
        let encoded =
            EncodedAction::encode(type_id, action).expect("a domain action is always serializable");
        self.last_dispatched = Some(encoded.clone());
        let mut transport = qos_actions::LocalTransport::new();
        if let Err(e) = qos_actions::dispatch(doc, action, encoded, &mut transport) {
            tracing::warn!(error = %e, type_id = %type_id, "action dispatch failed");
        }
    }

    /// Hand the host the heads captured before this event's first action, so it
    /// records one undo boundary bracketing them. Rising-edge: reading clears it,
    /// so it returns `Some` once per dispatched batch, then `None`. Wire this
    /// straight to
    /// [`Widget::take_committed_action`](crate::widget::Widget::take_committed_action).
    pub fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        self.commit_before.take()
    }

    /// The last action dispatched, for `affordance == ActionSpec` covenant tests:
    /// an app's own control and its Ctrl+K entry must build the byte-identical
    /// [`EncodedAction`].
    pub fn last_dispatched(&self) -> Option<&EncodedAction> {
        self.last_dispatched.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ReadDoc, ScalarValue, ROOT};
    use qos_actions::{ActionError, Doc, ExecutionLocus};
    use serde::{Deserialize, Serialize};

    /// A minimal real action: set a scalar under a key. Deterministic, no
    /// wall-clock / RNG — the executor's contract.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Put {
        key: String,
        value: i64,
    }

    const PUT_TYPE: &str = "test.put";

    impl Action for Put {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            doc.put(&ROOT, &self.key, ScalarValue::Int(self.value))?;
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    #[test]
    fn dispatch_applies_the_action_to_the_doc() {
        let mut doc = AutoCommit::new();
        let mut host = ActionHost::new();
        host.dispatch(&mut doc, PUT_TYPE, &Put { key: "score".into(), value: 42 });
        assert_eq!(doc.get(&ROOT, "score").unwrap().unwrap().0.to_i64(), Some(42));
    }

    #[test]
    fn brackets_one_undo_boundary_across_several_actions() {
        let mut doc = AutoCommit::new();
        // A pre-existing app write; the bracket must start *after* it.
        doc.put(&ROOT, "seed", ScalarValue::Str("s".into())).unwrap();
        let heads_before_actions = doc.get_heads();

        let mut host = ActionHost::new();
        host.dispatch(&mut doc, PUT_TYPE, &Put { key: "a".into(), value: 1 });
        host.dispatch(&mut doc, PUT_TYPE, &Put { key: "b".into(), value: 2 });

        // Only the first action of the event captures the boundary, and it is the
        // heads as of just before that first action (the flushed app write).
        let committed = host.take_committed().expect("first dispatch armed the boundary");
        assert_eq!(committed, heads_before_actions, "bracket covers only the actions");
        // Rising-edge: a second read yields nothing.
        assert!(host.take_committed().is_none(), "reading clears the boundary");
    }

    #[test]
    fn last_dispatched_records_the_encoded_action() {
        let mut doc = AutoCommit::new();
        let mut host = ActionHost::new();
        assert!(host.last_dispatched().is_none());
        let action = Put { key: "k".into(), value: 7 };
        host.dispatch(&mut doc, PUT_TYPE, &action);
        let enc = host.last_dispatched().expect("dispatch recorded the action");
        assert_eq!(enc, &EncodedAction::encode(PUT_TYPE, &action).unwrap());
    }
}
