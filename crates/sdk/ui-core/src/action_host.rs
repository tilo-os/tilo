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
use tilo_actions::{Action, ActionContext, EncodedAction};

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
        self.dispatch_encoded(doc, action, encoded, context)
    }

    /// [`Self::try_dispatch_with_context`] past encoding: one compiled copy
    /// for every action type.
    fn dispatch_encoded(
        &mut self,
        doc: &mut AutoCommit,
        action: &dyn Action,
        encoded: EncodedAction,
        context: Option<ActionContext>,
    ) -> Result<(), tilo_actions::ActionError> {
        let observed_before = crate::bound_dispatch::typed_action_before(doc);
        let before = observed_before.clone().unwrap_or_else(|| doc.get_heads());
        let mut transport = tilo_actions::LocalTransport::new();
        let actor = doc.get_actor().to_bytes().to_vec();
        let context = match (context, self.ambient_context.as_ref()) {
            (Some(context), Some(ambient)) => Some(context.enriched_by(ambient)),
            (None, Some(ambient)) => Some(ambient.clone()),
            (context, None) => context,
        };
        let observation = if matches!(action.change_kind(), tilo_actions::ChangeKind::External) {
            None
        } else {
            crate::local_document_action::reserve(&encoded, context.as_ref(), doc, &before)?
        };
        let captured_context = observation.as_ref().and_then(|_| context.clone());
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
        let dispatched = result?;
        if let Some(observation) = observation
            && matches!(dispatched, tilo_actions::Dispatched::AppliedLocal(_))
        {
            observation.finish(doc, &before, encoded.clone(), captured_context)?;
        }
        // Content from outside (fetched rows, a process's output) is nobody's
        // undo step: it opens no bracket and is left out of the open one.
        let step = action.change_kind().is_step();
        if let Some(before) = observed_before {
            let after = doc.get_heads();
            crate::bound_dispatch::report_typed_action(doc, before, after, step);
        }
        if step && self.commit_before.is_none() {
            self.commit_before = Some(before);
        }
        self.last_dispatched = Some(encoded);
        Ok(())
    }

    /// Rising-edge: reading clears it, so it returns `Some` once per
    /// dispatched batch, then `None`. Wire this straight to
    /// [`CommittedActions::take_committed_action`](crate::undo::CommittedActions::take_committed_action).
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
    use serde::{Deserialize, Serialize};
    use tilo_actions::{ActionError, Doc, ExecutionLocus};

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
                ActionContext::for_app("org.tilo.Test", "Test"),
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
                ActionContext::for_resource("counter:1", "counter", "Score"),
            );
            let heads = doc.get_heads();
            let change = doc
                .get_change_by_hash(&heads[0])
                .expect("contextual action committed");
            let context = change
                .message()
                .and_then(|message| ActionContext::from_change_message(message))
                .expect("context embedded in change");
            assert_eq!(
                context
                    .resource
                    .as_ref()
                    .map(|resource| resource.label.as_str()),
                Some("Score")
            );
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
                ActionContext::for_resource("counter", "counter", "Counter"),
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
    }
}
