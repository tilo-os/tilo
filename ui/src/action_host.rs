//! Framework-owned action dispatch every app gets by default.
//!
//! Adopting the qOS [action model](qos_actions) used to mean each app
//! copy-pasting the same undo-bracket, encode-and-dispatch,
//! last-dispatched, and no-op-transport plumbing. Hoisted here so an app
//! declares only its own domain and embeds one [`ActionHost`] field.
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
use qos_actions::{Action, ActionContext, EncodedAction};

/// Framework-owned action dispatch an app embeds as a single field, in
/// place of hand-rolled `perform_action`/`DiscardTransport`/
/// `last_dispatched` boilerplate.
///
/// The undo boundary this brackets carries no author: what crosses this
/// seam is heads, not identity. It's still reachable by interactive Ctrl+Z
/// because an author-scoped undo claims unattributed entries too — see
/// `UndoLog::claimable_by`.
#[derive(Default)]
pub struct ActionHost {
    /// Drained via
    /// [`Widget::take_committed_action`](crate::widget::Widget::take_committed_action)
    /// to record exactly one undo boundary bracketing the action(s).
    commit_before: Option<Vec<ChangeHash>>,
    /// Lets a test prove an affordance and the Ctrl+K
    /// [`ActionSpec`](qos_actions::ActionSpec) build the byte-identical
    /// action.
    last_dispatched: Option<EncodedAction>,
}

impl ActionHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Brackets the undo boundary on the *first* action of the event only
    /// (`get_heads` flushes pending app writes, so the bracket covers only
    /// the action's own change), encodes + remembers it as
    /// [`last_dispatched`](Self::last_dispatched), then dispatches through
    /// the default [`LocalTransport`](qos_actions::LocalTransport) — applied
    /// via the executor with a pinned actor, so it replays byte-identically
    /// on peers. A dispatch error is logged at WARN and swallowed; an
    /// affordance never panics the app.
    pub fn dispatch<A>(&mut self, doc: &mut AutoCommit, type_id: &str, action: &A)
    where
        A: Action + serde::Serialize,
    {
        self.dispatch_maybe_context(doc, type_id, action, None);
    }

    /// Dispatch with the activity context the app already has at the direct
    /// affordance: resource, human label, optional purpose/group, and browse
    /// facets. This is the preferred path for mutations that should appear in
    /// workspace history; [`Self::dispatch`] remains the legacy fallback.
    pub fn dispatch_with_context<A>(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &A,
        context: ActionContext,
    ) where
        A: Action + serde::Serialize,
    {
        self.dispatch_maybe_context(doc, type_id, action, Some(context));
    }

    fn dispatch_maybe_context<A>(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &A,
        context: Option<ActionContext>,
    ) where
        A: Action + serde::Serialize,
    {
        if self.commit_before.is_none() {
            self.commit_before = Some(doc.get_heads());
        }
        // Same "never panics the app" contract as the dispatch below: an
        // encoding failure is logged and swallowed rather than taking the
        // affordance down with it.
        let encoded = match EncodedAction::encode(type_id, action) {
            Ok(encoded) => encoded,
            Err(e) => {
                tracing::warn!(error = %e, type_id = %type_id, "action encoding failed");
                return;
            }
        };
        self.last_dispatched = Some(encoded.clone());
        let mut transport = qos_actions::LocalTransport::new();
        let result = match context {
            Some(context) => {
                qos_actions::dispatch_with_context(doc, action, encoded, &mut transport, context)
            }
            None => qos_actions::dispatch(doc, action, encoded, &mut transport),
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, type_id = %type_id, "action dispatch failed");
        }
    }

    /// Rising-edge: reading clears it, so it returns `Some` once per
    /// dispatched batch, then `None`. Wire this straight to
    /// [`Widget::take_committed_action`](crate::widget::Widget::take_committed_action).
    pub fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        self.commit_before.take()
    }

    pub fn last_dispatched(&self) -> Option<&EncodedAction> {
        self.last_dispatched.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue};
    use qos_actions::{ActionError, Doc, ExecutionLocus};
    use serde::{Deserialize, Serialize};

    /// Deterministic, no wall-clock / RNG — the executor's contract.
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
        host.dispatch(
            &mut doc,
            PUT_TYPE,
            &Put {
                key: "score".into(),
                value: 42,
            },
        );
        assert_eq!(
            doc.get(&ROOT, "score").unwrap().unwrap().0.to_i64(),
            Some(42)
        );
    }

    #[test]
    fn brackets_one_undo_boundary_across_several_actions() {
        let mut doc = AutoCommit::new();
        // A pre-existing app write; the bracket must start *after* it.
        doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        let heads_before_actions = doc.get_heads();

        let mut host = ActionHost::new();
        host.dispatch(
            &mut doc,
            PUT_TYPE,
            &Put {
                key: "a".into(),
                value: 1,
            },
        );
        host.dispatch(
            &mut doc,
            PUT_TYPE,
            &Put {
                key: "b".into(),
                value: 2,
            },
        );

        let committed = host
            .take_committed()
            .expect("first dispatch armed the boundary");
        assert_eq!(
            committed, heads_before_actions,
            "bracket covers only the actions"
        );
        assert!(
            host.take_committed().is_none(),
            "reading clears the boundary"
        );
    }

    #[test]
    fn last_dispatched_records_the_encoded_action() {
        let mut doc = AutoCommit::new();
        let mut host = ActionHost::new();
        assert!(host.last_dispatched().is_none());
        let action = Put {
            key: "k".into(),
            value: 7,
        };
        host.dispatch(&mut doc, PUT_TYPE, &action);
        let enc = host
            .last_dispatched()
            .expect("dispatch recorded the action");
        assert_eq!(enc, &EncodedAction::encode(PUT_TYPE, &action).unwrap());
    }

    #[test]
    fn contextual_dispatch_embeds_activity_in_the_change() {
        let mut doc = AutoCommit::new();
        let mut host = ActionHost::new();
        host.dispatch_with_context(
            &mut doc,
            PUT_TYPE,
            &Put {
                key: "score".into(),
                value: 9,
            },
            ActionContext::for_resource("Set score", "counter:1", "counter", "Score"),
        );
        let heads = doc.get_heads();
        let change = doc
            .get_change_by_hash(&heads[0])
            .expect("contextual action committed");
        let context = change
            .message()
            .and_then(|message| ActionContext::from_change_message(message))
            .expect("context embedded in change");
        assert_eq!(context.summary().as_deref(), Some("Set score · Score"));
    }
}
