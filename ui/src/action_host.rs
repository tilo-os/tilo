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
use qos_actions::{
    Action, ActionContext, ActionEnvelope, EncodedAction, OptimisticError, OptimisticQueue,
};

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
    ambient_context: Option<ActionContext>,
}

impl ActionHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set task/session provenance inherited by later dispatches. Concrete
    /// action/resource fields always win; this is primarily for purpose and
    /// work-group metadata supplied by an agent run or issue context.
    pub fn set_ambient_context(&mut self, context: Option<ActionContext>) {
        self.ambient_context = context;
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
        let actor = doc.get_actor().to_bytes().to_vec();
        let context = match (context, self.ambient_context.as_ref()) {
            (Some(context), Some(ambient)) => Some(context.enriched_by(ambient)),
            (None, Some(ambient)) => Some(ambient.clone()),
            (context, None) => context,
        };
        let result = match context {
            Some(context) => qos_actions::dispatch_by_with_context(
                doc,
                action,
                encoded,
                &mut transport,
                Some(&actor),
                context,
            ),
            None => qos_actions::dispatch_by(doc, action, encoded, &mut transport, Some(&actor)),
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, type_id = %type_id, "action dispatch failed");
        }
    }

    /// Apply an action immediately through an [`OptimisticQueue`] and return
    /// the envelope an app should send to its model host.
    ///
    /// This is the remote counterpart to [`Self::dispatch`]: it preserves the
    /// same undo bracket and inspection metadata, while acknowledgement,
    /// rejection, reconnect, and authoritative updates remain transport
    /// neutral in the queue. The widget API is unchanged because rendering
    /// continues to read the local `doc` replica.
    ///
    /// # Errors
    ///
    /// Returns an encoding or action-application error, or
    /// [`OptimisticError::LiveDiverged`] when `doc` was changed outside the
    /// active optimistic session.
    #[allow(
        clippy::result_large_err,
        reason = "callers need the structured optimistic/action error to distinguish rebase and transport recovery paths"
    )]
    pub fn dispatch_optimistic<A>(
        &mut self,
        doc: &mut AutoCommit,
        queue: &mut OptimisticQueue,
        type_id: &str,
        action: &A,
    ) -> Result<ActionEnvelope, OptimisticError>
    where
        A: Action + serde::Serialize,
    {
        let encoded = EncodedAction::encode(type_id, action)?;
        let before = doc.get_heads();
        let envelope = queue.submit(doc, action, encoded.clone())?;
        if self.commit_before.is_none() {
            self.commit_before = Some(before);
        }
        self.last_dispatched = Some(encoded);
        Ok(envelope)
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
    use qos_actions::{ActionError, ActionRegistry, Doc, ExecutionLocus};
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
    fn direct_actions_keep_the_clicking_documents_actor() {
        let actor = automerge::ActorId::from(vec![7, 8, 9, 10]);
        let mut doc = AutoCommit::new().with_actor(actor.clone());
        let mut host = ActionHost::new();
        host.dispatch_with_context(
            &mut doc,
            PUT_TYPE,
            &Put {
                key: "score".into(),
                value: 42,
            },
            ActionContext::for_action(
                "org.qos.Test",
                "Test",
                &Put {
                    key: "score".into(),
                    value: 42,
                },
            ),
        );
        let changes = doc.get_changes(&[]);
        assert_eq!(
            changes.last().map(automerge::Change::actor_id),
            Some(&actor)
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

    #[test]
    fn ambient_task_context_enriches_explicit_action_context() {
        let mut doc = AutoCommit::new();
        let mut host = ActionHost::new();
        host.set_ambient_context(Some(
            ActionContext::default()
                .with_purpose("Prepare launch")
                .with_group("issue:42", "Launch", "issue"),
        ));
        host.dispatch_with_context(
            &mut doc,
            PUT_TYPE,
            &Put {
                key: "k".into(),
                value: 1,
            },
            ActionContext::for_resource("Set", "counter", "counter", "Counter"),
        );
        let heads = doc.get_heads();
        let change = doc.get_change_by_hash(&heads[0]).unwrap();
        let context = change
            .message()
            .and_then(|message| ActionContext::from_change_message(message))
            .unwrap();
        assert_eq!(context.purpose.as_deref(), Some("Prepare launch"));
        assert_eq!(
            context.group.as_ref().map(|group| group.id.as_str()),
            Some("issue:42")
        );
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("counter")
        );
    }

    #[test]
    fn optimistic_dispatch_is_visible_and_keeps_the_action_host_contract() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "seed", true).unwrap();
        let before = doc.get_heads();
        let mut queue = OptimisticQueue::new(&mut doc);
        let mut host = ActionHost::new();
        let action = Put {
            key: "score".into(),
            value: 42,
        };

        let envelope = host
            .dispatch_optimistic(&mut doc, &mut queue, PUT_TYPE, &action)
            .unwrap();

        assert_eq!(
            doc.get(&ROOT, "score").unwrap().unwrap().0.to_i64(),
            Some(42),
            "the UI reads the optimistic value before a reply"
        );
        assert_eq!(host.take_committed(), Some(before));
        assert_eq!(host.last_dispatched(), Some(&envelope.action));
        assert_eq!(queue.reconnect_batch(), std::slice::from_ref(&envelope));

        let mut registry = ActionRegistry::new();
        registry.register::<Put>(PUT_TYPE);
        assert_eq!(
            queue.acknowledge(envelope.id, &registry).unwrap(),
            qos_actions::Acknowledgement::Accepted
        );
        assert!(queue.is_settled());
    }
}
