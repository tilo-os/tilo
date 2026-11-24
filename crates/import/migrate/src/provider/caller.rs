//! A device's migrations as their caller steps them: one invocation of a step, followed to its
//! end, and the questions a run asks its person answered as the caller says. `tilo migrate` is
//! this caller over the daemon's native endpoint; anything that reaches the device's dispatch
//! steps the migrations the same way.
//!
//! A run that keeps a mail account asks its person first: the call waits in its asked record,
//! which the workspace's invocations record lists (read through the record API) and which is
//! answered with its own `resolve`, as the person's shell answers it.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tilo_actions::remote::asked::{Asked, AskedBy, RESOLVE};
use tilo_actions::remote::workspace_invocations;
use tilo_actions::remote::{
    ActionProvider, Caller, InvokeRequest, MAX_REMOTE_WAIT, Pending, ReadAnswer, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget, ResourceReadRequest, UpdateMark,
};
use tilo_actions::{ActionAddress, ActionFailure, InvocationId, InvocationUpdate};

use super::{PROVIDER, resource};

/// How long past the longest remote wait the device is waited for before it is given up.
const SLACK: Duration = Duration::from_secs(30);

/// How long a run that may ask is waited on before its questions are looked for again.
const LOOK_AGAIN: Duration = Duration::from_millis(250);

/// The steps that land in a workspace, and so take the one they land in.
const LANDING: [&str; 6] = ["review", "choose", "start", "stop", "status", "undo"];

/// How a caller answers the questions a run asks: say yes beforehand (`--yes`), ask the person
/// where the caller runs, leave them to someone else, or (with nobody to ask) say no, so the
/// account stays unconnected and the run says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asks {
    /// Answer nobody: another client of the workspace (the person's shell) does.
    Leave,
    /// Say yes to every question, as `--yes` does.
    Approve,
    /// Say no: nobody is there to ask.
    Decline,
    /// Put each question to the person ([`Answering::prompt`]).
    Prompt,
}

impl Asks {
    /// Whether the caller answers the run's questions at all.
    #[must_use]
    pub const fn answers(self) -> bool {
        !matches!(self, Self::Leave)
    }
}

/// What the caller does with what a step says while it runs.
pub struct Answering<'a> {
    pub asks: Asks,
    /// Put a question to the person: their yes, or no. Asked only for [`Asks::Prompt`].
    pub prompt: &'a mut dyn FnMut(&Asked) -> Result<bool>,
    /// A line the person reads: the run's progress, a question asked of them, their answer.
    pub say: &'a mut dyn FnMut(&str),
}

/// One device's migrations, stepped by one caller through a dispatch that reaches the device.
pub struct Steps {
    dispatch: Arc<dyn ActionProvider + Send + Sync>,
    caller: Caller,
    /// The resource the device's migrations are.
    migrations: String,
    /// The workspace the device is in, and its name as the person knows it.
    workspace: String,
    workspace_name: Option<String>,
    /// Another workspace the device is a member of, which the migration lands in instead.
    into: Option<String>,
}

impl Steps {
    /// The migrations of the device `device`, stepped as `caller` through `dispatch`; the device
    /// is in `workspace`, which its person knows as `workspace_name`.
    #[must_use]
    pub fn new(
        dispatch: Arc<dyn ActionProvider + Send + Sync>,
        caller: Caller,
        device: &[u8; 32],
        workspace: String,
        workspace_name: Option<String>,
    ) -> Self {
        Self {
            dispatch,
            caller,
            migrations: resource(device),
            workspace,
            workspace_name,
            into: None,
        }
    }

    /// Land the migration in the workspace `named` names: the device's own by its id or name,
    /// or another by its id, which the device takes only once it is a member there. A name the
    /// device does not know is refused here, naming the command that joins it.
    ///
    /// # Errors
    /// `named` is neither the device's workspace nor a workspace id.
    pub fn in_workspace(&mut self, named: &str) -> Result<()> {
        if in_workspace(named, &self.workspace, self.workspace_name.as_deref()) {
            self.into = None;
            return Ok(());
        }
        if uuid::Uuid::parse_str(named).is_err() {
            return Err(not_in(named));
        }
        self.into = Some(named.to_owned());
        Ok(())
    }

    /// The workspace the migration lands in.
    fn landing(&self) -> &str {
        self.into.as_deref().unwrap_or(&self.workspace)
    }

    /// Invoke `action` with `args` and follow it to its end, answering the questions it asks as
    /// `answering` says: its answer, or the failure it ran into.
    ///
    /// # Errors
    /// The device did not answer, or refused the step before it ran.
    pub fn step(
        &self,
        action: &str,
        mut args: Map<String, Value>,
        answering: &mut Answering<'_>,
    ) -> Result<std::result::Result<Value, ActionFailure>> {
        if let Some(into) = self.into.as_ref().filter(|_| LANDING.contains(&action)) {
            args.insert("workspace".into(), json!(into));
        }
        let request = InvokeRequest::plain(
            RemoteActionTarget {
                provider: PROVIDER.into(),
                address: ActionAddress::new(self.migrations.clone(), action),
            },
            args,
        );
        let id = request.invocation;
        let receipt = answered(self.dispatch.invoke(&self.caller, request))?
            .map_err(|error| anyhow!("{action} was refused: {}", error.said()))?;
        self.follow(id, receipt.status, answering)
    }

    /// Follow the invocation `id`, whose receipt said `status`, to its end, answering the
    /// questions it asks as `answering` says.
    fn follow(
        &self,
        id: InvocationId,
        mut status: ReceiptStatus,
        answering: &mut Answering<'_>,
    ) -> Result<std::result::Result<Value, ActionFailure>> {
        let stopped = || anyhow!("the device stopped answering");
        let mut seen = UpdateMark::Accepted;
        loop {
            match status {
                ReceiptStatus::Accepted => {}
                ReceiptStatus::Completed { output } => {
                    return Ok(Ok(output.value.unwrap_or_default()));
                }
                ReceiptStatus::Failed { error } => return Ok(Err(error)),
                ReceiptStatus::Rejected { error } => bail!("{}", error.said()),
                ReceiptStatus::Cancelled { message } => {
                    return Ok(Ok(json!({
                        "said": message.unwrap_or_else(|| "it was stopped".into()),
                    })));
                }
            }
            // Asked for before looking, so a change between the two is never missed.
            let change = self
                .dispatch
                .next_change(&self.caller, &id)
                .ok_or_else(stopped)?;
            let state = self
                .dispatch
                .observe(&self.caller, &id)
                .ok_or_else(stopped)?;
            if UpdateMark::of(&state.update) == seen {
                if answering.asks.answers() {
                    // A run that waits for its person asks in a record of its own: the caller
                    // answers it, as any other client of the workspace does.
                    self.answer_asks(answering)?;
                    let _ = change.wait_timeout(LOOK_AGAIN);
                } else {
                    // A wait the device let pass unanswered is looked at again.
                    let _ = change.wait_timeout(MAX_REMOTE_WAIT.saturating_add(SLACK));
                }
                continue;
            }
            seen = UpdateMark::of(&state.update);
            if let InvocationUpdate::Progress {
                message: Some(line),
                ..
            } = &state.update
            {
                (answering.say)(line);
            }
            status = state.receipt().status;
        }
    }

    /// Answer the questions a run asks, as `answering` says, each with its record's own
    /// `resolve`, as the person's shell answers it.
    fn answer_asks(&self, answering: &mut Answering<'_>) -> Result<()> {
        for question in self.questions()? {
            if !connects_a_mail_account(&question) {
                continue;
            }
            let approve = match answering.asks {
                Asks::Approve => true,
                Asks::Decline => false,
                Asks::Prompt => (answering.prompt)(&question)?,
                Asks::Leave => continue,
            };
            if answering.asks != Asks::Prompt {
                (answering.say)(&question_said(&question));
            }
            if let Err(error) = self.answer(&question, approve) {
                // Another client answered it first: the run reads their answer.
                (answering.say)(&format!("the answer did not go through: {error:#}"));
                continue;
            }
            (answering.say)(if approve {
                "You said yes: it is connected."
            } else {
                "You said no: it stays unconnected."
            });
        }
        Ok(())
    }

    /// The questions that wait for the caller's person where the migration lands, oldest first:
    /// the asked invocations its invocations record lists, read through the record API.
    ///
    /// # Errors
    /// The record could not be read.
    pub fn questions(&self) -> Result<Vec<Asked>> {
        let reads = self
            .dispatch
            .resource_reads()
            .ok_or_else(|| anyhow!("the device reads no records"))?;
        let read = ResourceReadRequest {
            workspace: self.landing().to_owned(),
            app: workspace_invocations::PROVIDER.to_owned(),
            record: String::new(),
            joined: false,
            after: None,
            wait_ms: None,
            answer: ReadAnswer::Reading,
        };
        let reading = match answered(reads.answer_read(&self.caller, 0, read))? {
            RemoteActionMessage::ResourceReading { result, .. } => result
                .map_err(|error| anyhow!("the questions could not be read: {}", error.said()))?,
            other => bail!("the device answered no reading: {other:?}"),
        };
        let reading: Value = serde_json::from_str(&reading)?;
        Ok(waiting_in(&reading))
    }

    /// Answer `question`'s record with `approve`, in the workspace that keeps it.
    fn answer(&self, question: &Asked, approve: bool) -> Result<()> {
        let mut args = Map::new();
        args.insert(
            "resolution".into(),
            json!(if approve { "approve" } else { "decline" }),
        );
        let request = InvokeRequest::plain(
            RemoteActionTarget {
                provider: workspace_invocations::PROVIDER.into(),
                address: ActionAddress::new(question.record.record.clone(), RESOLVE),
            },
            args,
        );
        let caller = self
            .caller
            .clone()
            .in_workspace(question.record.workspace.clone());
        answered(self.dispatch.invoke(&caller, request))?
            .map_err(|error| anyhow!("the answer was refused: {}", error.said()))?;
        Ok(())
    }
}

/// The questions a reading of a workspace's invocations holds: the asked calls that wait for
/// their person, oldest first.
fn waiting_in(reading: &Value) -> Vec<Asked> {
    let mut asked: Vec<Asked> = reading
        .pointer("/records/0/value/items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|invocation| invocation["status"] == "waiting")
        .filter_map(|invocation| serde_json::from_value(invocation.get("asked")?.clone()).ok())
        .collect();
    asked.sort_by_key(|asked| asked.asked_at_ms);
    asked
}

/// What the device answers to `pending`.
fn answered<T: Send + 'static>(pending: Pending<T>) -> Result<T> {
    pending
        .wait_timeout(MAX_REMOTE_WAIT.saturating_add(SLACK))
        .ok()
        .flatten()
        .ok_or_else(|| anyhow!("the device did not answer"))
}

/// Whether `named` is the workspace the device is in, by its id or the name its person knows it
/// under.
fn in_workspace(named: &str, workspace: &str, name: Option<&str>) -> bool {
    named == workspace || name.is_some_and(|name| name.eq_ignore_ascii_case(named))
}

/// The refusal of a workspace the device is not in: the command that joins it is the one to
/// name, so a person has the step to take.
pub(crate) fn not_in(named: &str) -> anyhow::Error {
    anyhow!(
        "this computer's device is not in the workspace {named:?}: join it with the invitation \
         link a member sends you, `tilo login <invitation link>` (a server of yours takes it in \
         with `tilo login --server`), then run `tilo migrate -w {named}`"
    )
}

/// Whether `question` asks to keep a mail account: the only question a migration's run asks.
#[must_use]
pub fn connects_a_mail_account(question: &Asked) -> bool {
    question.action.ends_with("#add")
}

/// `question` as the person reads it before answering: who asks, in their words, and what is
/// decided.
#[must_use]
pub fn question_said(question: &Asked) -> String {
    let facts = &question.facts;
    let mut said = if facts.words.trim().is_empty() {
        match &facts.on {
            Some(on) if !on.is_empty() => format!("{} {on}", facts.verb),
            _ => facts.verb.clone(),
        }
    } else {
        facts.words.clone()
    };
    if let AskedBy::Agent(agent) = &facts.by {
        let _ = write!(said, " (asked by {})", agent.name);
    }
    for fact in &facts.facts {
        let _ = write!(said, "\n  {}: {}", fact.label, fact.value);
    }
    said
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use tilo_actions::remote::asked::{AskedFacts, Fact, Named, RecordRef};

    use super::*;

    const ENTERED: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";

    fn asked(action: &str, facts: AskedFacts) -> Asked {
        Asked {
            record: RecordRef {
                workspace: ENTERED.into(),
                record: "7".into(),
                device: None,
            },
            workspace: ENTERED.into(),
            workspace_name: ENTERED.into(),
            app: String::new(),
            action: action.into(),
            name: String::new(),
            asked_at_ms: 0,
            pressed: None,
            offers: Vec::new(),
            facts,
        }
    }

    /// A workspace the device is in is named by its id or by the name the person knows it under;
    /// one it is not in is not, and the refusal names the command that joins it.
    #[test]
    fn a_workspace_the_device_is_in_is_named_by_id_or_name() {
        assert!(in_workspace(ENTERED, ENTERED, Some("Studio")));
        assert!(in_workspace("studio", ENTERED, Some("Studio")));
        assert!(!in_workspace("Lab", ENTERED, Some("Studio")));
        assert!(!in_workspace("studio", ENTERED, None));

        let refusal = format!("{}", not_in("Lab"));
        assert!(refusal.contains("tilo login"), "{refusal}");
        assert!(refusal.contains("Lab"), "{refusal}");
    }

    /// Only the question that keeps a mail account is a migration's: the caller answers that
    /// one, and leaves another question to its own caller.
    #[test]
    fn only_keeping_a_mail_account_is_a_migrations_question() {
        let facts = AskedFacts::default();
        assert!(connects_a_mail_account(&asked(
            "workspace/mail-accounts#add",
            facts.clone()
        )));
        assert!(!connects_a_mail_account(&asked(
            "workspace/mail-accounts#list",
            facts
        )));
    }

    /// The question reads as the person answers it: who asks, what it is on, and what is decided.
    #[test]
    fn a_question_reads_as_the_person_answers_it() {
        let said = question_said(&asked(
            "workspace/mail-accounts#add",
            AskedFacts {
                by: AskedBy::Agent(Named {
                    id: "agent:migrate".into(),
                    name: "Migrations".into(),
                }),
                verb: "connect".into(),
                on: Some("ada@example.com".into()),
                facts: vec![
                    Fact::new("receives from", "imap.example.com:993"),
                    Fact::new("signs in with", "ada@example.com"),
                ],
                ..AskedFacts::default()
            },
        ));
        assert!(said.contains("connect ada@example.com"), "{said}");
        assert!(said.contains("asked by Migrations"), "{said}");
        assert!(said.contains("imap.example.com:993"), "{said}");
        assert!(said.contains("signs in with"), "{said}");
    }

    /// The questions are the asked calls of the workspace's invocations that wait for their
    /// person, oldest first; an invocation that ended asks nothing.
    #[test]
    fn the_questions_are_the_invocations_that_wait() {
        let mut later = asked("workspace/mail-accounts#add", AskedFacts::default());
        later.asked_at_ms = 20;
        let mut earlier = later.clone();
        earlier.asked_at_ms = 10;
        earlier.record.record = "6".into();
        let waiting = |asked: &Asked| json!({"status": "waiting", "asked": asked});
        let ended = json!({"status": "ended", "asked": later});
        let reading = json!({"revision": "r", "records": [{"record": "", "value": {
            "items": [waiting(&later), ended, waiting(&earlier)], "total": 3, "waiting": 2,
        }}]});
        let questions = waiting_in(&reading);
        let records: Vec<&str> = questions.iter().map(|q| q.record.record.as_str()).collect();
        assert_eq!(records, ["6", "7"]);
    }
}
