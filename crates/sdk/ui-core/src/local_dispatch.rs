//! The local executor every immediate action goes through.
//!
//! A bound view's dispatcher hands each action of its resource to
//! [`LocalDispatch`] through [`Edits::apply`](crate::widget::Edits::apply): it
//! runs on the canonical executor with this device's actor, is reported to the
//! collaboration route, and opens the undo boundary the host takes with
//! [`LocalDispatch::take_committed`].

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::{Action, ActionContext, EncodedAction};

/// The local executor, held by whatever applies actions to a document.
///
/// The undo boundary this brackets carries no author: what crosses this
/// seam is heads, not identity. It's still reachable by interactive Ctrl+Z
/// because an author-scoped undo claims unattributed entries too — see
/// `UndoLog::claimable_by`.
#[derive(Default)]
pub struct LocalDispatch {
    /// Drained via [`Self::take_committed`] to record exactly one undo
    /// boundary bracketing the action(s).
    commit_before: Option<Vec<ChangeHash>>,
}

impl LocalDispatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// [`Self::try_dispatch_with_context`] without an activity context.
    #[cfg(test)]
    pub(crate) fn try_dispatch<A>(
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

    /// Apply `action` now under `context`, which names it in the workspace
    /// history. Records the undo boundary only once the action applies.
    ///
    /// # Errors
    ///
    /// Returns the encoding, document or application refusal error; no undo
    /// boundary is recorded then.
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
        let change_kind = action.change_kind();
        let observation = if matches!(change_kind, tilo_actions::ChangeKind::External) {
            None
        } else {
            crate::local_document_action::reserve(&encoded, context.as_ref(), doc, &before)?
        };
        let captured_context = observation.as_ref().and_then(|_| context.clone());
        // Without an authenticated publication, only an execution that changes
        // the document makes this input's history unsigned.
        let unsigned = if observation.is_none()
            && !matches!(change_kind, tilo_actions::ChangeKind::External)
        {
            crate::local_document_action::unsigned_input(context.as_ref())
                .map(|edits| (edits, doc.get_heads()))
        } else {
            None
        };
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
        // An action may commit changes and still return an error. The
        // executor rolls back only its open transaction, so a committed edit
        // survives the refusal; inspect the document before surfacing that
        // error, or the surviving edit would leave history falsely signed.
        if let Some((edits, heads)) = unsigned
            && doc.get_heads() != heads
        {
            crate::local_document_action::note_unsigned_edit(&edits);
        }
        let dispatched = result?;
        if let Some(observation) = observation
            && matches!(dispatched, tilo_actions::Dispatched::AppliedLocal(_))
        {
            observation.finish(doc, &before, encoded.clone(), captured_context)?;
        }
        // Content from outside (fetched rows, a process's output) is nobody's
        // undo step: it opens no bracket and is left out of the open one.
        let step = change_kind.is_step();
        if let Some(before) = observed_before {
            let after = doc.get_heads();
            crate::bound_dispatch::report_typed_action(doc, before, after, step);
        }
        if step && self.commit_before.is_none() {
            self.commit_before = Some(before);
        }
        Ok(())
    }

    /// Rising-edge: reading clears it, so it returns `Some` once per
    /// dispatched batch, then `None`. Wire this straight to
    /// [`CommittedActions::take_committed_action`](crate::undo::CommittedActions::take_committed_action).
    pub fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        self.commit_before.take()
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
        {
            let mut doc = AutoCommit::new();
            let mut host = LocalDispatch::new();
            host.try_dispatch(
                &mut doc,
                PUT_TYPE,
                &Put {
                    key: "score".into(),
                    value: 42,
                },
            )
            .unwrap();
            assert_eq!(
                doc.get(&ROOT, "score").unwrap().unwrap().0.as_i64(),
                Some(42)
            );
        }

        {
            let actor = automerge::ActorId::from(vec![7, 8, 9, 10]);
            let mut doc = AutoCommit::new().with_actor(actor.clone());
            let mut host = LocalDispatch::new();
            host.try_dispatch_with_context(
                &mut doc,
                PUT_TYPE,
                &Put {
                    key: "score".into(),
                    value: 42,
                },
                ActionContext::for_app("org.tilo.Test", "Test"),
            )
            .unwrap();
            let changes = doc.get_changes(&[]);
            assert_eq!(
                changes.last().map(automerge::Change::actor_id),
                Some(&actor)
            );
        }

        {
            let mut doc = AutoCommit::new();
            // A pre-existing app write; the bracket must start *after* it.
            doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
                .unwrap();
            let heads_before_actions = doc.get_heads();

            let mut host = LocalDispatch::new();
            host.try_dispatch(
                &mut doc,
                PUT_TYPE,
                &Put {
                    key: "a".into(),
                    value: 1,
                },
            )
            .unwrap();
            host.try_dispatch(
                &mut doc,
                PUT_TYPE,
                &Put {
                    key: "b".into(),
                    value: 2,
                },
            )
            .unwrap();

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

        {
            let mut doc = AutoCommit::new();
            let mut host = LocalDispatch::new();
            assert!(matches!(
                host.try_dispatch(&mut doc, "test.refuse", &Refuse),
                Err(tilo_actions::ActionError::Refused(_))
            ));
            assert!(host.take_committed().is_none());
            assert!(doc.get_heads().is_empty());
        }
    }

    #[test]
    fn dispatch_context_and_optimism_keep_the_action_host_contract() {
        {
            let mut doc = AutoCommit::new();
            let mut host = LocalDispatch::new();
            host.try_dispatch_with_context(
                &mut doc,
                PUT_TYPE,
                &Put {
                    key: "score".into(),
                    value: 9,
                },
                ActionContext::for_resource("counter:1", "counter", "Score"),
            )
            .unwrap();
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
    }
}
