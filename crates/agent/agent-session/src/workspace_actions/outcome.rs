//! What an invocation's confirmation, caller and receipt read as, and which
//! records an app's replica opens as its own: the rules the queue, the bounded
//! calls and the tests all ask of the same request; and what an invocation's
//! outcome is recorded and answered as.

use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueQuery, Confirmation, InvocationReceipt, InvocationState,
    ReceiptStatus,
};
use tilo_actions::{
    ActionFailure, ActionOutput, ConfirmationPolicy, InvocationId, InvocationUpdate,
};

use super::Performed;
use super::publish::Unsaved;

use crate::record_actions::Outcome;

/// Whether `given`, from the host's own person on their own device, confirms
/// an action asking `policy`: their own word, the phrase it names, or their
/// approve of the asked record this very invocation was kept as, which only
/// the dispatch carries. A token of anyone else's confirms nothing here.
pub(super) fn confirms(policy: &ConfirmationPolicy, given: Option<&Confirmation>) -> bool {
    match (policy, given) {
        (_, Some(Confirmation::Asked(_))) => true,
        (_, Some(Confirmation::Approved(_))) => false,
        (ConfirmationPolicy::Phrase { phrase }, Some(Confirmation::Phrase(given))) => {
            phrase == given
        }
        (ConfirmationPolicy::Phrase { .. }, _) | (_, None) => false,
        (_, Some(Confirmation::Confirmed | Confirmation::Phrase(_))) => true,
    }
}

pub(super) fn exact_view_query(
    query: &CatalogueQuery,
) -> Result<(&tilo_actions::remote::ViewResourceQuery, &str), ActionCallError> {
    let wanted = query
        .view_resource
        .as_ref()
        .ok_or(ActionCallError::UnknownAction)?;
    let provider = query
        .provider
        .as_deref()
        .ok_or(ActionCallError::UnknownAction)?;
    if wanted.id.is_empty()
        || wanted.id.trim().is_empty()
        || wanted.id.chars().any(char::is_control)
        || wanted.id.len() > 2048
        || wanted.kind.is_empty()
        || wanted.kind.trim().is_empty()
        || wanted.kind.chars().any(char::is_control)
        || wanted.kind.len() > 128
        || provider.len() > 2048
        || provider.chars().any(char::is_control)
        || query.target.is_some()
        || !query.cursor.is_empty()
        || query.parent_resource.is_some()
    {
        return Err(ActionCallError::UnknownAction);
    }
    Ok((wanted, provider))
}

/// Whether `caller` speaks for someone a workspace may admit as a member: a
/// person themselves, the agent a device of theirs handed its session to (an
/// agent's grant), which every decision then holds to what an agent of
/// theirs may do, or an agent device they certified, held to what the
/// workspace grants that agent. The workspace's governance still decides
/// whether it admits them.
pub(super) fn speaks_for_a_member(caller: &Caller) -> bool {
    if caller.acting().is_person() {
        return true;
    }
    if !caller.acting().is_agent() {
        return false;
    }
    if caller.is_session_delegated() {
        return caller.acting_for_person().is_some();
    }
    caller.user_public_key().is_some() && caller.device_public_key().is_some()
}

/// The receipt of an invocation the host turns away outright, with why.
pub(super) fn rejected(invocation: InvocationId, error: ActionCallError) -> InvocationReceipt {
    InvocationReceipt {
        invocation,
        status: ReceiptStatus::Rejected { error },
    }
}

/// Log how the action `target` names ended: succeeded, failed or refused.
pub(super) fn log_outcome(
    target: &tilo_actions::remote::RemoteActionTarget,
    outcome: &Result<(ActionOutput, Option<tilo_actions::remote::ActorPlace>), Outcome>,
) {
    match outcome {
        Ok(_) => {
            tracing::info!(provider = %target.provider, resource = %target.address.resource,
            action = %target.address.action, "workspace actions: succeeded");
        }
        Err(Outcome::Failed(error)) => tracing::info!(provider = %target.provider,
            resource = %target.address.resource, action = %target.address.action,
            code = %error.code, message = %error.message, "workspace actions: failed"),
        Err(Outcome::Rejected(error)) => tracing::info!(provider = %target.provider,
            resource = %target.address.resource, action = %target.address.action,
            code = error.code(), %error, "workspace actions: refused"),
    }
}

/// Whether `record` is one of `app`'s own records, which its replica opens. An app that keeps one
/// record per workspace opens that one, and what it keeps as records of its own beside it (a
/// chain's parts); a person's record of some kind, kept beside the app's for that person (Files'
/// tree of the workspace's folder, and the chunks that tree is kept in), is read where it is kept,
/// never mounted as the app's, which refuses it as not the app's workspace record. So is a
/// document the app keeps for the whole workspace: its replica publishes it over the record the
/// app keeps.
pub(super) fn opens_as_its_own(app: &ui::application::ApplicationDefinition, record: &str) -> bool {
    // The empty route is the app's own record where it keeps one per workspace, and its index of
    // the workspace's named records where it keeps none: the index is read where it is kept,
    // never mounted as the app's record.
    if record.is_empty() {
        return app.keeps_workspace_resource();
    }
    // A tree's own record and its chunks are one folder, read record by record where it is kept.
    if fs_protocol::tree::chunk_of_record(record).is_some() {
        return false;
    }
    // A document the app keeps for the whole workspace beside its own record is published by the
    // app's replica over the record it keeps, never mounted on its own.
    if collab_types::workspace_document(app.app_id(), record).is_some() {
        return false;
    }
    !app.keeps_workspace_resource() || collab_types::person_resource_type(record).is_none()
}

/// Whether a failure is this invocation's answer. One the caller may ask
/// again is not: the same request under the same id executes it again rather
/// than being handed the failure it already has, which is what a change the
/// workspace took none of because the session it rode was replaced needs.
pub(super) fn settles(error: &ActionFailure) -> bool {
    !error.retryable
}

/// The outcome of an action whose change could not be committed.
pub(super) fn commit_failed(message: String, retryable: bool) -> Outcome {
    Outcome::Failed(ActionFailure {
        code: "commit_failed".into(),
        message,
        details: None,
        retryable,
    })
}

impl super::Worker {
    /// Record what `outcome` answered for `invocation` and answer it: done
    /// once what it wrote is saved, or the failure or refusal it ended with.
    pub(super) fn settle(
        &self,
        invocation: InvocationId,
        outcome: Performed,
        unsaved: Option<Unsaved>,
    ) -> InvocationReceipt {
        let outcome = match (outcome, unsaved) {
            (Ok(done), Some(unsaved)) => return self.answer_pending(invocation, done, unsaved),
            (outcome, _) => outcome,
        };
        let mut log = self.log();
        log.running.remove(&invocation);
        let (update, left_at) = match outcome {
            Ok((output, left_at)) => (InvocationUpdate::Succeeded { invocation, output }, left_at),
            Err(Outcome::Failed(error)) if !settles(&error) => {
                // A failure the caller may ask again is not this
                // invocation's answer: remembered as the failure it was, so
                // whoever follows the invocation learns to ask again, and the
                // same request under the same id makes it again rather than
                // being handed the failure it already has.
                let update = InvocationUpdate::Failed {
                    invocation,
                    error: error.clone(),
                };
                log.answered(invocation, &update, None);
                return InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Failed { error },
                };
            }
            Err(Outcome::Failed(error)) => (InvocationUpdate::Failed { invocation, error }, None),
            // Answered accepted while the person was asked: it ends refused,
            // and stays to say so.
            Err(Outcome::Rejected(error))
                if log
                    .entries
                    .get(&invocation)
                    .is_some_and(|entry| entry.asked) =>
            {
                let error = ActionFailure {
                    code: error.code().to_owned(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                };
                (InvocationUpdate::Failed { invocation, error }, None)
            }
            // A refusal the caller may ask again about (the document has not
            // arrived from the workspace yet) is remembered as failed, so a
            // caller following the invocation learns to ask again; the same
            // request under the same id then makes it.
            Err(Outcome::Rejected(error)) if error.ask_again() => {
                let error = ActionFailure {
                    code: error.code().to_owned(),
                    message: error.to_string(),
                    details: None,
                    retryable: true,
                };
                (InvocationUpdate::Failed { invocation, error }, None)
            }
            Err(Outcome::Rejected(error)) => {
                log.forget(&invocation);
                return rejected(invocation, error);
            }
        };
        log.answered(invocation, &update, left_at);
        drop(log);
        InvocationState::new(invocation, update).receipt()
    }
}
