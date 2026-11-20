//! What an agent working inside a turn wants to do and its person's grants
//! leave to them, decided with nobody at a screen.
//!
//! A turn a server takes for a person works with every device of theirs off, so
//! a question put to their devices would lapse. Two things take its place.
//!
//! A lasting permission: where the person answered a question about exactly
//! this action with always, and has not taken that back
//! ([`WorkspaceDocumentsLink::stands`](super::WorkspaceDocumentsLink::stands)),
//! the invocation goes as it would on their answer now, under a token bound
//! to that one invocation, its target and its arguments.
//!
//! A question in the conversation: otherwise the server keeps what was asked
//! and puts it to the person in the conversation the turn is in, where it
//! waits for as long as they take ([`AsksInConversation::ask`]).
//! The invocation ends at once as asked. Nothing ran, the turn goes on and
//! ends, and nothing is held while the question waits. The server does it from
//! the person's own answer ([`super::WorkspaceActions::serve_allowed`]).
//!
//! Either way what is decided is what the system names: the app, the kind of
//! resource, the action, the document and the resource in it
//! ([`Asked`](tilo_actions::remote::person_asking::Asked)). Nothing the agent
//! wrote decides it.
//!
//! An always is for the action that asked, never for what carried it: an
//! invocation that carries its resource's other actions is asked about by
//! their names, offers no always, and goes ahead unasked only where the own
//! always of each carried action that asks stands ([`super::carried`]).

use super::bounded::About;
use super::link::{AskedInTurn, AsksInConversation};
use super::publish::Unsaved;
use super::{
    ActionCallError, Arc, Caller, Confirmation, InvocationId, InvokeRequest, Outcome, Performed,
    Worker,
};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{InvocationReceipt, InvocationState, Pending, RemoteActionMessage};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_server_protocol::WorkspaceId;

/// What an invocation an agent made inside a turn ends with once its person
/// is asked about it in the conversation: nothing ran, and the agent neither
/// waits nor asks again.
const ASKED_IN_CONVERSATION: &str = "Your person is asked about this in the conversation, and it \
     is done when they allow it there. Nothing ran yet: tell them it waits for their answer, and do \
     not make this call again.";
/// What it ends with when the question could not be put.
const NOT_ASKED: &str = "Your person could not be asked about this in the conversation";
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

/// How `invocation` ends once the question about it was put to its person in the
/// conversation, or could not be (`put`): nothing ran either way, and it is told which.
/// The end of a question that was put says so typed as well: its person is asked and their
/// answer does it, which whoever words this end tells from one where nobody was asked.
fn ended(invocation: InvocationId, put: Option<&Result<(), String>>) -> InvocationUpdate {
    let message = match put {
        Some(Ok(())) => ASKED_IN_CONVERSATION.to_owned(),
        Some(Err(why)) => format!("{NOT_ASKED} ({why}), so nothing ran."),
        None => format!("{NOT_ASKED}, so nothing ran."),
    };
    let mut update = failed(invocation, message);
    if matches!(put, Some(Ok(())))
        && let InvocationUpdate::Failed { error, .. } = &mut update
    {
        error.details = Some(tilo_actions::remote::person_asking::asked_detail());
    }
    update
}

/// Whether `outcome` is an invocation that waits for its person's answer.
const fn waits(outcome: &Performed) -> bool {
    matches!(
        outcome,
        Err(Outcome::Rejected(
            ActionCallError::ConfirmationRequired { .. }
        ))
    )
}

impl Worker {
    /// Do `request` for `caller` and save what it changed: how it went and
    /// what is not saved yet. Where it waits for its person, a lasting
    /// permission of theirs lets it through, and otherwise they are asked: in the
    /// conversation for an agent inside a turn, else on their own devices.
    ///
    /// # Errors
    /// The receipt of an invocation whose person is being asked.
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
        let (mut outcome, mut unsaved, mut about) = self.performed(workspace_id, caller, request);
        if waits(&outcome)
            && let Some(waiting) = about.as_deref()
            && let Some(permissions) = self.stands_for(workspace_id, caller, request, waiting)
        {
            // What let it through is kept for the invocation, counted once it
            // is done even where the attempt let through has to wait first.
            if let Some(entry) = self.log().entries.get_mut(&request.invocation) {
                entry.stood = permissions;
            }
            (outcome, unsaved, about) = self.performed(workspace_id, caller, request);
        }
        if waits(&outcome)
            && let Some(about) = about
            && let Some(receipt) = self
                .ask_in_conversation(workspace_id, caller, request, &about)
                .or_else(|| self.ask_person(workspace_id, caller, request, &about))
        {
            return Err(receipt);
        }
        if outcome.is_ok()
            && let Some(link) = self.shared.link.get()
        {
            // What let its person's action through is counted once it is
            // done, also where the attempt let through had to wait first.
            let stood = self
                .log()
                .entries
                .get_mut(&request.invocation)
                .map(|entry| std::mem::take(&mut entry.stood))
                .unwrap_or_default();
            for permission in &stood {
                link.permission_used(workspace_id, caller, permission);
            }
        }
        Ok((outcome, unsaved))
    }

    /// Do `request` and save what it changed: how it went, what is not
    /// saved yet, and what its person is asked where it waits for them.
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

    /// The lasting permissions of the person `caller` acts for that cover
    /// `request`, by their ids, for an agent working inside a turn: the one
    /// for the action itself, or, for an invocation that carries its
    /// resource's other actions, the own one of every carried action that asks
    /// by its own name ([`About::opened_by`]). With every one of them the
    /// server holds that person's answer for this invocation as it would had
    /// they just allowed it: a token bound to the invocation, its target and
    /// its arguments. `None` where one of them does not stand.
    pub(super) fn stands_for(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        about: &About,
    ) -> Option<Vec<String>> {
        caller.turn()?;
        self.bounded(caller)?;
        if request.confirmation.is_some() {
            return None;
        }
        let workspace = self
            .workspaces
            .get(&workspace_id)
            .and_then(|workspace| workspace.cluster_id(workspace_id))?;
        let agent = caller.acting().agent_id()?;
        let opened_by = about.opened_by(agent, &workspace, request)?;
        let link = self.shared.link.get()?;
        let mut permissions = Vec::with_capacity(opened_by.len());
        for asked in &opened_by {
            permissions.push(link.stands(workspace_id, caller, asked)?);
        }
        // Nothing that stands opens nothing.
        if permissions.is_empty() {
            return None;
        }
        let token = self.shared.token_for(workspace_id, caller, request)?;
        self.log().entries.get_mut(&request.invocation)?.approval = Some(token);
        Some(permissions)
    }

    /// Put `request`, which `caller` (an agent working inside a turn) asked
    /// for and which needs its person's confirmation, to that person as a
    /// question in the conversation: answered accepted, it ends as asked
    /// once the server kept the question, and the server does it from their
    /// answer. `None` when it is not asked there: the caller is in no turn or
    /// gave a confirmation of its own, the arguments hold something a draft
    /// withholds (a server keeps no secret while a question waits), the person
    /// was asked already, or nothing on this server asks in a conversation.
    /// An invocation a question could not show whole (a list or a text
    /// longer than a draft holds) is answered too: refused, with nobody
    /// asked.
    pub(super) fn ask_in_conversation(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        about: &About,
    ) -> Option<InvocationReceipt> {
        caller.turn()?;
        self.bounded(caller)?;
        if request.confirmation.is_some() {
            return None;
        }
        let invocation = request.invocation;
        let workspace = self
            .workspaces
            .get(&workspace_id)
            .and_then(|workspace| workspace.cluster_id(workspace_id))?;
        let agent = caller.acting().agent_id()?;
        let draft = about.draft();
        if draft
            .fields
            .iter()
            .any(|field| field.value == tilo_actions::DraftValue::Withheld)
        {
            return None;
        }
        // What its person cannot be shown whole they are not asked about,
        // here or on a device: it is not done.
        if let Some(label) = shortened(&draft) {
            let update = failed(invocation, format!("{NOT_SHOWN}: {label}."));
            if let Some(entry) = self.log().entries.get_mut(&invocation) {
                entry.update = update.clone();
            }
            return Some(InvocationState::new(invocation, update).receipt());
        }
        let conversations = self.shared.in_conversation.read_recover().clone()?;
        if self
            .log()
            .entries
            .get_mut(&invocation)
            .is_none_or(|entry| std::mem::replace(&mut entry.asked, true))
        {
            return None;
        }
        let in_turn = AskedInTurn {
            request: request.clone(),
            draft,
            asked: about.always(agent, &workspace, request),
            words: about.words(),
        };
        let Some(put) = conversations.ask(workspace_id, caller, in_turn) else {
            if let Some(entry) = self.log().entries.get_mut(&invocation) {
                entry.asked = false;
            }
            return None;
        };
        let shared = Arc::clone(&self.shared);
        put.then(move |put| shared.asked_in_conversation(invocation, put.as_ref()));
        Some(InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }).receipt())
    }
}

impl super::Shared {
    /// A token that lets `request` go ahead once for `caller`: bound to that one invocation,
    /// its target and its arguments.
    fn token_for(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<String> {
        self.approvals
            .issue(super::bounded::ApprovalBinding::of(
                workspace_id,
                caller,
                request,
            ))
            .ok()
    }

    /// The question about `invocation` was put to its person in the
    /// conversation, or could not be (`put`): either way the invocation ends
    /// here, told which, and nothing ran.
    fn asked_in_conversation(&self, invocation: InvocationId, put: Option<&Result<(), String>>) {
        let update = ended(invocation, put);
        let mut log = self.log.lock_recover();
        if let Some(entry) = log.entries.get_mut(&invocation)
            && !entry.update.is_terminal()
        {
            entry.update = update;
        }
        // Whoever follows the invocation waits for it to move.
        log.moved();
        drop(log);
    }
}

impl super::WorkspaceActions {
    /// Put what an agent working inside a turn asks its person to them in
    /// the conversation, through `asker`, from now on: the runtime that works
    /// agents on this server gives itself. Until one is given, such an agent's
    /// person is asked on their own devices like everybody's.
    pub fn ask_in_conversations(&self, asker: Arc<dyn AsksInConversation>) {
        *self.shared.in_conversation.write_recover() = Some(asker);
    }

    /// Do `request` for `caller` as the person `caller` acts for allowed
    /// it: the server read their own answer to the question it put to them in
    /// the conversation ([`AsksInConversation::ask`]), and confirms this one
    /// invocation, its target and its arguments, and
    /// nothing else. Only the server calls this, with an answer it read
    /// itself: no request from outside reaches it. `None` when the server's
    /// actions are not running.
    #[must_use]
    pub fn serve_allowed(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        mut request: InvokeRequest,
    ) -> Option<Pending<RemoteActionMessage>> {
        let token = self.shared.token_for(workspace_id, caller, &request)?;
        request.confirmation = Some(Confirmation::Approved(token));
        self.serve(workspace_id, caller, RemoteActionMessage::Invoke(request))
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

    /// An invocation whose person was asked about it in the conversation ends with nothing
    /// done, under the code of a confirmation that is required and in its own sentence, and
    /// says, typed, that a question was put: its end is no failure of the action. Where no
    /// question could be put it says no such thing.
    #[test]
    fn an_invocation_whose_person_was_asked_says_so_typed() {
        use tilo_actions::remote::{CONFIRMATION_REQUIRED_CODE, person_asking};

        let invocation = InvocationId::new();
        let end = |put: Option<&Result<(), String>>| match ended(invocation, put) {
            InvocationUpdate::Failed { error, .. } => error,
            other => panic!("it ends with nothing done: {other:?}"),
        };
        let told = end(Some(&Ok(())));
        assert_eq!(told.code, CONFIRMATION_REQUIRED_CODE);
        assert_eq!(told.message, ASKED_IN_CONVERSATION);
        assert!(person_asking::says_asked(told.details.as_ref()), "{told:?}");

        let unput: Result<(), String> = Err("no conversation".to_owned());
        for put in [Some(&unput), None] {
            let told = end(put);
            assert_eq!(told.code, CONFIRMATION_REQUIRED_CODE);
            assert!(told.message.starts_with(NOT_ASKED), "{told:?}");
            assert!(
                !person_asking::says_asked(told.details.as_ref()),
                "{told:?}"
            );
        }
    }
}
