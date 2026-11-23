//! What an invocation the caller's grants leave to their person ends with: refused for want
//! of the person's yes, showing them what would be done ([`About::facts`]). The dispatch keeps
//! the call as its own asked invocation record, and the person's approve admits it again,
//! carrying their answer ([`tilo_actions::remote::InvokeRequest::approval`]). Nothing here keeps
//! the question, waits for it, or asks a device.
//!
//! What is shown is what the system names: the app, the resource type, the action, the record
//! and the call as its person reviews it. Nothing the caller wrote decides it. A call the
//! person could not be shown whole (a list or a text longer than a draft holds) is refused
//! with nobody asked.

use super::bounded::About;
use super::publish::Unsaved;
use super::{
    ActionCallError, Arc, Caller, InvocationId, InvokeRequest, Outcome, Performed, Worker,
};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{InvocationReceipt, InvocationState};
use tilo_server_protocol::WorkspaceId;

/// What it ends with when its asked record could not be written.
const NOT_ASKED: &str = "Your person could not be asked about this, so nothing ran";
/// What it ends with when a question could not show its person all of it.
const NOT_SHOWN: &str = "This holds more than a question shows your person, so they were not \
     asked and nothing ran. Do it in smaller parts, each of which they can see whole. What was \
     too long";

/// The first field of `draft` that shows less than its invocation holds: a
/// list longer than a draft lists, or a text longer than a draft keeps. A
/// person shown such a draft would allow what they did not see.
fn shortened(draft: &tilo_actions::Draft) -> Option<&str> {
    draft.fields.iter().find_map(|field| {
        let cut = match &field.value {
            tilo_actions::DraftValue::List { count, labels } => *count > labels.len(),
            tilo_actions::DraftValue::Text { text } => {
                !field.editable && text.chars().count() >= tilo_actions::draft::MAX_FIELD_CHARS
            }
            _ => false,
        };
        cut.then_some(field.label.as_str())
    })
}

/// `invocation` ended with nothing done, told `message`.
fn failed(invocation: InvocationId, message: String) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: tilo_actions::api::ApiError::ConfirmationRequired
                .code()
                .to_owned(),
            message,
            details: None,
            retryable: false,
        },
    }
}

impl Worker {
    /// Do `request` for `caller` and save what it changed: how it went and what is not saved
    /// yet. Where it waits for its person, it is refused showing them what would be done.
    ///
    /// # Errors
    /// The receipt of an invocation its person could not be shown whole.
    #[allow(
        clippy::result_large_err,
        reason = "the receipt is the answer the caller documents"
    )]
    pub(super) fn invoke_or_ask(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(Performed, Option<Unsaved>), InvocationReceipt> {
        let (outcome, unsaved, about) = self.performed(workspace_id, caller, request);
        match (outcome, about) {
            (Err(Outcome::Rejected(ActionCallError::ConfirmationRequired(_))), Some(about)) => {
                Err(self.asked(workspace_id, caller, request, &about))
            }
            // A provider that leaves the call to the person says itself what they are shown;
            // one that refuses a call they approved is not asked about again.
            (Err(Outcome::Rejected(ActionCallError::ConfirmationRequired(facts))), None)
                if request.approval().is_none() =>
            {
                Err(self.keep_asked(workspace_id, caller, request, *facts))
            }
            (outcome, _) => Ok((outcome, unsaved)),
        }
    }

    /// Do `request` and save what it changed: how it went, what is not
    /// saved yet, and what its person is shown where it waits for them.
    fn performed(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> (Performed, Option<Unsaved>, Option<Box<About>>) {
        let performed = self.perform(workspace_id, caller, request);
        self.note_written(request.invocation);
        let opens = self.opened_resource(request).is_some();
        let (outcome, unsaved) = self.saved(workspace_id, performed, opens);
        let about = self
            .log()
            .entries
            .get_mut(&request.invocation)
            .and_then(|entry| entry.about.take());
        (outcome, unsaved, about)
    }

    /// The receipt of `request`, which `caller` asked for and which waits for its person:
    /// kept as its asked invocation record and accepted, its update naming the record, until
    /// the person's approve admits it again; refused for want of their yes, showing them
    /// `about`, where no store here keeps asked records, so whoever answers the caller keeps
    /// it; refused outright where they could not be shown all of it.
    pub(super) fn asked(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        about: &About,
    ) -> InvocationReceipt {
        let invocation = request.invocation;
        if let Some(label) = shortened(&about.draft()) {
            return self.ended(
                invocation,
                failed(invocation, format!("{NOT_SHOWN}: {label}.")),
            );
        }
        self.keep_asked(workspace_id, caller, request, about.facts(caller))
    }

    /// The receipt of `request`, which waits for its person's yes, showing them `facts`: kept
    /// as its asked record and accepted, or refused for want of their yes where no store here
    /// keeps asked records.
    pub(super) fn keep_asked(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        facts: tilo_actions::remote::asked::AskedFacts,
    ) -> InvocationReceipt {
        let invocation = request.invocation;
        let workspace = self
            .workspaces
            .get(&workspace_id)
            .and_then(|workspace| workspace.cluster_id(workspace_id));
        let kept = self
            .shared
            .asked_records
            .get()
            .zip(workspace)
            .map(|(records, at)| records.write_asked(&at, caller, request, &facts));
        match kept {
            Some(Ok(record)) => {
                if let Some(entry) = self.log().entries.get_mut(&invocation) {
                    entry.asked = true;
                }
                self.ended(invocation, record.waiting(invocation))
            }
            Some(Err(why)) => self.ended(
                invocation,
                failed(invocation, format!("{NOT_ASKED} ({why}).")),
            ),
            None => {
                self.log().forget(&invocation);
                InvocationReceipt {
                    invocation,
                    status: tilo_actions::remote::ReceiptStatus::Rejected {
                        error: ActionCallError::asks(facts),
                    },
                }
            }
        }
    }

    /// `invocation` stands at `update` now: its receipt.
    fn ended(&self, invocation: InvocationId, update: InvocationUpdate) -> InvocationReceipt {
        let mut log = self.log();
        if let Some(entry) = log.entries.get_mut(&invocation) {
            entry.update = update.clone();
        }
        log.moved();
        drop(log);
        InvocationState::new(invocation, update).receipt()
    }
}

impl super::WorkspaceActions {
    /// Keep every invocation that waits for a person's yes as its asked record through
    /// `records`: the workspace's store, which the host hands the service once.
    pub fn asked_records(&self, records: Arc<dyn tilo_actions::remote::asked::AskedRecords>) {
        let _ = self.shared.asked_records.set(records);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{Draft, DraftField, DraftValue};

    fn draft_of(value: DraftValue, editable: bool) -> Draft {
        Draft {
            action_label: "Send".into(),
            resource_label: "Outbox".into(),
            fields: vec![DraftField {
                path: vec!["to".into()],
                label: "To".into(),
                value,
                editable,
                multiline: false,
                shows: None,
                role: None,
            }],
            ..Draft::default()
        }
    }

    /// A draft that lists fewer entries than its list holds, or keeps less
    /// of a text than was given, shows its person less than would be done: it is
    /// named, so the invocation is refused instead of asked. One that shows
    /// everything is not.
    #[test]
    fn a_draft_that_shows_less_than_would_be_done_is_named() {
        let names = |count: usize| (0..count).map(|n| format!("p{n}")).collect::<Vec<_>>();
        let listed_whole = DraftValue::List {
            count: 12,
            labels: names(12),
        };
        assert_eq!(shortened(&draft_of(listed_whole, false)), None);
        let listed_in_part = DraftValue::List {
            count: 13,
            labels: names(12),
        };
        assert_eq!(shortened(&draft_of(listed_in_part, false)), Some("To"));

        let short = DraftValue::Text {
            text: "bo@example.test".into(),
        };
        assert_eq!(shortened(&draft_of(short, true)), None);
        let cut = DraftValue::Text {
            text: "a".repeat(tilo_actions::draft::MAX_FIELD_CHARS),
        };
        assert_eq!(shortened(&draft_of(cut, false)), Some("To"));
        let flag = DraftValue::Flag { on: true };
        assert_eq!(shortened(&draft_of(flag, true)), None);
    }
}
