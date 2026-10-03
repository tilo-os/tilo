//! Framework-owned action dispatch every app gets by default.
//!
//! [`ActionHost`] centralizes dispatch, undo-boundary, and test-observation
//! bookkeeping so an app declares only its own action domain.
//!
//! ```ignore
//! struct MyApp { actions: tilo_ui::ActionHost, /* … */ }
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
use tilo_actions::{
    Action, ActionContext, ActionEnvelope, EncodedAction, OptimisticError, OptimisticQueue,
};

/// Framework-owned action dispatch an app embeds as a single field.
///
/// The undo boundary this brackets carries no author: what crosses this
/// seam is heads, not identity. It's still reachable by interactive Ctrl+Z
/// because an author-scoped undo claims unattributed entries too — see
/// `UndoLog::claimable_by`.
#[derive(Default)]
pub struct ActionHost {
    /// Drained via
    /// [`CommittedActions::take_committed_action`](crate::undo::CommittedActions::take_committed_action)
    /// to record exactly one undo boundary bracketing the action(s).
    commit_before: Option<Vec<ChangeHash>>,
    /// Lets a test prove an affordance and the Ctrl+K
    /// [`ActionSpec`](tilo_actions::ActionSpec) build the byte-identical
    /// action.
    last_dispatched: Option<EncodedAction>,
    /// Compensating document action for outside-consequence undo, computed
    /// by [`Action::inverse_before`](tilo_actions::Action::inverse_before)
    /// before the forward action ran. Cleared with [`Self::take_committed`].
    pending_undo_via: Option<EncodedAction>,
    /// The forward action paired with [`Self::pending_undo_via`], for redo.
    pending_redo_via: Option<EncodedAction>,
    ambient_context: Option<ActionContext>,
}

impl ActionHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Authorize a direct widget affordance using its actual published action
    /// contract and the current canonical component's live permission context.
    /// Unbound native widgets retain their existing host policy.
    ///
    /// # Errors
    /// Refuses missing/revoked owned custody or an action removed by the same
    /// permission filter used for named resource actions.
    pub fn authorize_resource_action(
        doc: &AutoCommit,
        resource: &tilo_actions::Resource,
        action_id: &str,
    ) -> Result<(), String> {
        crate::bound_dispatch::authorize_current_resource_action(doc, resource, action_id)
    }

    /// Set task/session provenance inherited by later dispatches. Concrete
    /// action/resource fields always win; this is primarily for purpose and
    /// work-group metadata supplied by an agent run or issue context.
    pub fn set_ambient_context(&mut self, context: Option<ActionContext>) {
        self.ambient_context = context;
    }

    /// Brackets the undo boundary on the *first* successful action of the event only
    /// (`get_heads` flushes pending app writes, so the bracket covers only
    /// the action's own change), encodes + remembers it as
    /// [`last_dispatched`](Self::last_dispatched), then dispatches through
    /// the default [`LocalTransport`](tilo_actions::LocalTransport) — applied
    /// via the executor with a pinned actor, so it replays byte-identically
    /// on peers. A dispatch error is logged at WARN and swallowed; an
    /// affordance never panics the app.
    pub fn dispatch<A>(&mut self, doc: &mut AutoCommit, type_id: &str, action: &A)
    where
        A: Action + serde::Serialize,
    {
        if let Err(error) = self.try_dispatch(doc, type_id, action) {
            tracing::warn!(error = %error, type_id = %type_id, "action dispatch failed");
        }
    }

    /// Dispatch an action through the canonical local executor and return a
    /// structured refusal or infrastructure error to the affordance.
    ///
    /// Unlike [`dispatch`](Self::dispatch), this does not swallow errors. It
    /// only records an undo boundary and last-dispatched payload after the
    /// action applies.
    ///
    /// # Errors
    ///
    /// Returns an encoding, document, or application refusal error without
    /// recording an undo boundary or last-dispatched payload.
    pub fn try_dispatch<A>(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &A,
    ) -> Result<(), tilo_actions::ActionError>
    where
        A: Action + serde::Serialize,
    {
        self.try_dispatch_maybe_context(doc, type_id, action, None)
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
        if let Err(error) = self.try_dispatch_with_context(doc, type_id, action, context) {
            tracing::warn!(error = %error, type_id = %type_id, "action dispatch failed");
        }
    }

    /// Contextual [`try_dispatch`](Self::try_dispatch). Resource/history
    /// provenance follows the same canonical executor path as every other
    /// direct affordance.
    ///
    /// # Errors
    ///
    /// Returns the same encoding, document, or application refusal errors as
    /// [`try_dispatch`](Self::try_dispatch), without recording success state.
    pub fn try_dispatch_with_context<A>(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &A,
        context: ActionContext,
    ) -> Result<(), tilo_actions::ActionError>
    where
        A: Action + serde::Serialize,
    {
        self.try_dispatch_maybe_context(doc, type_id, action, Some(context))
    }

    fn try_dispatch_maybe_context<A>(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &A,
        context: Option<ActionContext>,
    ) -> Result<(), tilo_actions::ActionError>
    where
        A: Action + serde::Serialize,
    {
        let encoded = EncodedAction::encode(type_id, action)?;
        // Capture the compensating action against the pre-apply document so a
        // delete can still read the row it removes.
        let undo_via = action.inverse_encoded(doc).or_else(|| {
            action
                .inverse_before(doc)
                .and_then(|inverse| EncodedAction::encode(type_id, &inverse).ok())
        });
        let observed_before = crate::bound_dispatch::typed_action_before(doc);
        let before = observed_before.clone().unwrap_or_else(|| doc.get_heads());
        let mut transport = tilo_actions::LocalTransport::new();
        let actor = doc.get_actor().to_bytes().to_vec();
        let context = match (context, self.ambient_context.as_ref()) {
            (Some(context), Some(ambient)) => Some(context.enriched_by(ambient)),
            (None, Some(ambient)) => Some(ambient.clone()),
            (context, None) => context,
        }
        .map(|context| context.with_args_from_payload(&encoded.payload));
        let result = match context {
            Some(context) => tilo_actions::dispatch_by_with_context(
                doc,
                action,
                encoded.clone(),
                &mut transport,
                Some(&actor),
                context,
            ),
            None => tilo_actions::dispatch_by(
                doc,
                action,
                encoded.clone(),
                &mut transport,
                Some(&actor),
            ),
        };
        result?;
        if let Some(before) = observed_before {
            let after = doc.get_heads();
            crate::bound_dispatch::report_typed_action(doc, before, after);
        }
        if self.commit_before.is_none() {
            self.commit_before = Some(before);
        }
        // One outside-consequence action per bracket: a later pure CRDT edit
        // in the same event must not erase a pending compensating undo.
        if undo_via.is_some() && self.pending_undo_via.is_none() {
            self.pending_undo_via = undo_via;
            self.pending_redo_via = Some(encoded.clone());
        }
        self.last_dispatched = Some(encoded);
        Ok(())
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
        let observed_before = crate::bound_dispatch::typed_action_before(doc);
        let before = observed_before.clone().unwrap_or_else(|| doc.get_heads());
        let envelope = queue.submit(doc, action, encoded.clone())?;
        if let Some(before) = observed_before {
            let after = doc.get_heads();
            crate::bound_dispatch::report_typed_action(doc, before, after);
        }
        if self.commit_before.is_none() {
            self.commit_before = Some(before);
        }
        self.last_dispatched = Some(encoded);
        Ok(envelope)
    }

    /// Rising-edge: reading clears it, so it returns `Some` once per
    /// dispatched batch, then `None`. Wire this straight to
    /// [`CommittedActions::take_committed_action`](crate::undo::CommittedActions::take_committed_action).
    pub fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        self.commit_before.take()
    }

    /// Rising-edge pair for outside-consequence undo: `(undo_via, redo_via)`.
    /// Cleared with the commit bracket so a host records them on the same
    /// [`UndoEntry`](crate::state::UndoEntry).
    pub fn take_action_revert(&mut self) -> Option<(EncodedAction, EncodedAction)> {
        match (self.pending_undo_via.take(), self.pending_redo_via.take()) {
            (Some(undo), Some(redo)) => Some((undo, redo)),
            (undo, redo) => {
                self.pending_undo_via = undo;
                self.pending_redo_via = redo;
                None
            }
        }
    }

    pub fn last_dispatched(&self) -> Option<&EncodedAction> {
        self.last_dispatched.as_ref()
    }

    /// Drop any recorded dispatch without applying a new action. Used when an
    /// outside-consequence path deliberately emits no collaborative write
    /// (e.g. VPN local credential setup) so observers do not confuse a prior
    /// seed publish with this operation.
    pub fn clear_last_dispatched(&mut self) {
        self.last_dispatched = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue};
    use serde::{Deserialize, Serialize};
    use tilo_actions::{ActionError, ActionRegistry, Doc, ExecutionLocus};

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

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Refuse;

    impl Action for Refuse {
        fn apply(&self, _doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            Err(tilo_actions::ActionError::Refused("not allowed".into()))
        }

        fn locus(&self) -> tilo_actions::ExecutionLocus {
            tilo_actions::ExecutionLocus::ClientLocal
        }
    }

    #[test]
    fn dispatch_applies_actions_as_one_undo_boundary_with_the_clicking_actor() {
        // Case `dispatch_applies_the_action_to_the_doc`.
        {
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
                doc.get(&ROOT, "score").unwrap().unwrap().0.as_i64(),
                Some(42)
            );
        }

        // Case `direct_actions_keep_the_clicking_documents_actor`.
        {
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
                    "org.tilo.Test",
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

        // Case `brackets_one_undo_boundary_across_several_actions`.
        {
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

        // Case `last_dispatched_records_the_encoded_action`.
        {
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

        // Case `try_dispatch_refusal_leaves_no_undo_boundary_or_last_payload`.
        {
            let mut doc = AutoCommit::new();
            let mut host = ActionHost::new();
            assert!(matches!(
                host.try_dispatch(&mut doc, "test.refuse", &Refuse),
                Err(tilo_actions::ActionError::Refused(_))
            ));
            assert!(host.take_committed().is_none());
            assert!(host.last_dispatched().is_none());
            assert!(doc.get_heads().is_empty());
        }
    }

    #[test]
    fn dispatch_context_and_optimism_keep_the_action_host_contract() {
        // Case `contextual_dispatch_embeds_activity_in_the_change`.
        {
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

        // Case `ambient_task_context_enriches_explicit_action_context`.
        {
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

        // Case `optimistic_dispatch_is_visible_and_keeps_the_action_host_contract`.
        {
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
                doc.get(&ROOT, "score").unwrap().unwrap().0.as_i64(),
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
                tilo_actions::Acknowledgement::Accepted
            );
            assert!(queue.is_settled());
        }
    }
}
