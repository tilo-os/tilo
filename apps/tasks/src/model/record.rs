//! The tracker: every issue of the workspace, and the verbs on them.

use tilo_rich_text::RichText;
use ui::tilo_actions::ActorKind;
use ui::tilo_resource::{self, ActionCx, Collection, Counter, Id, Model, Refusal, Set, resource};

use crate::model::issue::{
    AgentRequest, Imported, Issue, Link, MAX_MESSAGE_CHARS, Message, MessageKind, Status,
};
use crate::model::run::{
    Budget, Confirmation, ConfirmationState, ModelChoice, Question, Run, RunOutcome, RunPhase,
};

/// A workspace's issues.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Tracker {
    /// Issues
    ///
    /// Flat, in the order the board shows them: a sub-issue names its
    /// parent. Each issue is a storage unit of its own.
    #[model(unit)]
    pub issues: Collection<Issue>,
    /// The highest number given so far. Increments merge, so a number given
    /// after a merge is past every issue either side made.
    #[model(readonly)]
    pub numbers: Counter,
    /// Tasks of the workspace's room already imported, by their id: an
    /// imported task is never imported again, even after its issue is
    /// deleted.
    #[model(readonly)]
    pub imported: Set<String>,
}

fn no_issue() -> Refusal {
    Refusal::new("This issue no longer exists")
}

impl Tracker {
    /// The issue numbered `number`, if one is.
    #[must_use]
    pub fn by_number(&self, number: u64) -> Option<(&Id, &Issue)> {
        self.issues.iter().find(|(_, issue)| issue.number == number)
    }

    /// The direct sub-issues of `parent` (top-level issues for `None`), in
    /// board order.
    pub fn children<'a>(
        &'a self,
        parent: Option<&'a Id>,
    ) -> impl Iterator<Item = (&'a Id, &'a Issue)> {
        self.issues
            .iter()
            .filter(move |(_, issue)| issue.parent.as_ref() == parent)
    }

    /// How many of `parent`'s direct sub-issues are closed, of how many.
    /// Counted, never stored: a parent's status is its own.
    #[must_use]
    pub fn progress(&self, parent: &Id) -> (usize, usize) {
        self.children(Some(parent))
            .fold((0, 0), |(closed, total), (_, issue)| {
                (
                    closed.saturating_add(usize::from(issue.status.closed())),
                    total.saturating_add(1),
                )
            })
    }

    /// Whether `issue` is `ancestor` or sits somewhere under it.
    #[must_use]
    pub fn is_under(&self, issue: &Id, ancestor: &Id) -> bool {
        let mut at = Some(issue.clone());
        // A parent chain longer than the tracker is a cycle a merge made:
        // stop rather than loop.
        for _ in 0..=self.issues.len() {
            match at {
                Some(id) if &id == ancestor => return true,
                Some(id) => at = self.issues.get(&id).and_then(|issue| issue.parent.clone()),
                None => return false,
            }
        }
        false
    }

    fn issue_mut(&mut self, issue: &Id) -> Result<&mut Issue, Refusal> {
        self.issues.get_mut(issue).ok_or_else(no_issue)
    }

    fn next_number(&mut self) -> u64 {
        self.numbers.add(1);
        u64::try_from(self.numbers.get()).unwrap_or_default()
    }

    fn check_parent(&self, parent: Option<&Id>) -> Result<(), Refusal> {
        match parent {
            Some(parent) if !self.issues.contains(parent) => {
                Err(Refusal::new("The parent issue no longer exists"))
            }
            _ => Ok(()),
        }
    }
}

/// The invoker, when a person: the run verbs a person answers refuse agents.
fn person(cx: &ActionCx) -> Result<String, Refusal> {
    match (cx.actor_kind(), cx.author()) {
        (None | Some(ActorKind::Person), Some(author)) => Ok(author.to_owned()),
        (Some(_), _) => Err(Refusal::new("Only a person can do this")),
        (None, None) => Err(Refusal::new("Sign in to do this")),
    }
}

fn author(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .map(str::to_owned)
        .ok_or_else(|| Refusal::new("Sign in to do this"))
}

fn by_agent(cx: &ActionCx) -> bool {
    matches!(cx.actor_kind(), Some(ActorKind::Agent))
}

fn message_text(text: &str) -> Result<String, Refusal> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Refusal::new("Write a message first"));
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(Refusal::new("This message is too long"));
    }
    Ok(text.to_owned())
}

/// The run of `issue` going on, invoked by its own agent. The door that
/// admits the agent's actions holds them to the run they belong to.
fn own_run<'a>(cx: &ActionCx, issue: &'a mut Issue) -> Result<&'a mut Run, Refusal> {
    let agent = cx.author().unwrap_or_default().to_owned();
    let run = issue
        .run
        .as_mut()
        .filter(|run| run.phase.active())
        .ok_or_else(|| Refusal::new("This run has ended or was replaced"))?;
    if !by_agent(cx) || run.agent.0 != agent {
        return Err(Refusal::new("Only the run's own agent reports on it"));
    }
    Ok(run)
}

fn push_message(issue: &mut Issue, cx: &mut ActionCx, message: Message) {
    let id = cx.id();
    issue.comments.push(id, message);
}

#[resource(kind = "tasks.tracker", label = "Tasks", search = crate::model::search::entries)]
impl Tracker {
    /// Create an issue.
    ///
    /// # Errors
    /// Refuses a blank title, a parent that no longer exists, or an invalid
    /// assignee.
    #[action(writes = issues)]
    pub fn create(
        &mut self,
        cx: &mut ActionCx,
        #[param("What the issue is about, in a line")] title: String,
        #[param("The issue it is a sub-issue of")] parent: Option<Id>,
        #[param("Its description, in markdown")] body: Option<String>,
        #[param("Where it starts; Backlog without")] status: Option<Status>,
        #[param("person:<id> or agent:<id>")] assignee: Option<String>,
    ) -> Result<(), Refusal> {
        self.check_parent(parent.as_ref())?;
        let author = author(cx)?;
        let number = self.next_number();
        let issue = Issue {
            number,
            title: title.trim().to_owned(),
            body: RichText::from_markdown(body.unwrap_or_default()),
            status: status.unwrap_or_default(),
            assignee: assignee.map(ui::tilo_resource::Atom),
            parent,
            author,
            created_ms: cx.now_ms(),
            ..Issue::default()
        };
        let id = cx.id();
        self.issues.push(id, issue);
        Ok(())
    }

    /// Move an issue under another one, or to the top level.
    ///
    /// # Errors
    /// Refuses moving an issue under itself or one of its own sub-issues.
    #[action(writes = issues)]
    pub fn move_under(&mut self, issue: &Id, parent: Option<Id>) -> Result<(), Refusal> {
        self.check_parent(parent.as_ref())?;
        if let Some(parent) = &parent
            && self.is_under(parent, issue)
        {
            return Err(Refusal::new(
                "An issue cannot sit under itself or its own sub-issues",
            ));
        }
        self.issue_mut(issue)?.parent = parent;
        Ok(())
    }

    /// Assign an issue to a person or an agent, or to nobody.
    ///
    /// # Errors
    /// Refuses while an agent's run of the issue is going, or an assignee
    /// that is neither `person:<id>` nor `agent:<id>`.
    #[action(writes = issues)]
    pub fn assign(
        &mut self,
        issue: &Id,
        #[param("person:<id> or agent:<id>; nobody without")] assignee: Option<String>,
    ) -> Result<(), Refusal> {
        let target = self.issue_mut(issue)?;
        if target.active_run().is_some() {
            return Err(Refusal::new(
                "Stop the run before assigning the issue to someone else",
            ));
        }
        target.assignee = assignee
            .map(|assignee| assignee.trim().to_owned())
            .filter(|assignee| !assignee.is_empty())
            .map(ui::tilo_resource::Atom);
        Ok(())
    }

    /// Choose the model the issue's agent plans with, or let it plan with
    /// the one chosen for the agent.
    ///
    /// # Errors
    /// Refuses while a run of the issue is going, or a choice that names no
    /// provider or model.
    #[action(writes = issues)]
    pub fn choose_model(&mut self, issue: &Id, model: Option<ModelChoice>) -> Result<(), Refusal> {
        if model
            .as_ref()
            .is_some_and(|model| model.profile.trim().is_empty() || model.model.trim().is_empty())
        {
            return Err(Refusal::new("A model names its provider and itself"));
        }
        let target = self.issue_mut(issue)?;
        if target.active_run().is_some() {
            return Err(Refusal::new("Change the model when no run is going"));
        }
        target.model = model;
        Ok(())
    }

    /// Ask an agent to do something: an issue assigned to it whose run
    /// starts at once, approved by the asking itself.
    ///
    /// # Errors
    /// Refuses an id already taken, a blank title, an invoker who is not a
    /// person, or an agent that is not named.
    #[action(audience = "people", writes = issues)]
    pub fn ask_agent(
        &mut self,
        cx: &mut ActionCx,
        #[param(flatten)] ask: AgentRequest,
    ) -> Result<(), Refusal> {
        let starter = person(cx)?;
        if self.issues.contains(&ask.id) {
            return Err(Refusal::new("That issue exists already"));
        }
        let agent = crate::model::agent_assignee(ask.agent.trim());
        if agent.0.trim_start_matches("agent:").is_empty() {
            return Err(Refusal::new("Name the agent to ask"));
        }
        let number = self.next_number();
        let now = cx.now_ms();
        let issue = Issue {
            number,
            title: ask.title.trim().to_owned(),
            body: RichText::from_markdown(ask.body.unwrap_or_default()),
            status: Status::InProgress,
            assignee: Some(agent.clone()),
            model: ask.model.clone(),
            subject: ask.subject,
            author: starter.clone(),
            created_ms: now,
            run: Some(Run {
                epoch: 1,
                agent,
                starter,
                phase: RunPhase::Approved,
                model: ask.model,
                started_ms: now,
                updated_ms: now,
                confirmation: None,
            }),
            ..Issue::default()
        };
        self.issues.push(ask.id, issue);
        Ok(())
    }

    /// Label an issue.
    ///
    /// # Errors
    /// Refuses a blank label, or an issue at its 32 labels.
    #[action(writes = issues)]
    pub fn add_label(&mut self, issue: &Id, label: String) -> Result<(), Refusal> {
        let label = label.trim();
        if label.is_empty() {
            return Err(Refusal::new("A label has a name"));
        }
        self.issue_mut(issue)?.labels.insert(label.to_owned());
        Ok(())
    }

    /// Place an issue on the board: in a status column, before another
    /// issue or at the end.
    ///
    /// # Errors
    /// Refuses an issue that no longer exists.
    #[action(writes = issues)]
    pub fn place(
        &mut self,
        issue: &Id,
        status: Status,
        #[param("The issue it goes before; the end of the column without")] before: Option<Id>,
    ) -> Result<(), Refusal> {
        self.issue_mut(issue)?.status = status;
        let position = before
            .filter(|before| before != issue)
            .and_then(|before| self.issues.position(&before));
        let from = self.issues.position(issue).unwrap_or_default();
        let to = match position {
            // Taking the issue out first shifts what was after it.
            Some(at) if at > from => at.saturating_sub(1),
            Some(at) => at,
            None => self.issues.len(),
        };
        self.issues.move_to(issue, to);
        Ok(())
    }

    /// Comment on an issue.
    ///
    /// # Errors
    /// Refuses an empty message.
    #[action(field = "comments", writes = issues)]
    pub fn comment(&mut self, cx: &mut ActionCx, issue: &Id, text: String) -> Result<(), Refusal> {
        let text = message_text(&text)?;
        let author = author(cx)?;
        let agent = by_agent(cx);
        let at_ms = cx.now_ms();
        let target = self.issue_mut(issue)?;
        push_message(
            target,
            cx,
            Message {
                author,
                by_agent: agent,
                at_ms,
                text,
                ..Message::default()
            },
        );
        Ok(())
    }

    /// Link an issue to another resource.
    ///
    /// # Errors
    /// Refuses a link that names no resource.
    #[action(writes = issues)]
    pub fn link(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The app that keeps the resource")] app: String,
        #[param("The resource's kind")] kind: String,
        #[param("The resource's address in its app")] resource: String,
        #[param("How the link reads")] label: String,
        #[param("The app's record that keeps it; its record of the workspace without")]
        record: Option<String>,
    ) -> Result<(), Refusal> {
        if resource.trim().is_empty() || app.trim().is_empty() {
            return Err(Refusal::new("A link names a resource"));
        }
        let id = cx.id();
        self.issue_mut(issue)?.links.push(
            id,
            Link {
                label,
                app,
                kind,
                resource,
                record: record.filter(|record| !record.is_empty()),
            },
        );
        Ok(())
    }

    /// Delete an issue. Its sub-issues move up to its parent.
    ///
    /// # Errors
    /// Refuses an issue that no longer exists.
    #[action(destructive, writes = issues)]
    pub fn delete(&mut self, issue: &Id) -> Result<(), Refusal> {
        let removed = self.issues.remove(issue).ok_or_else(no_issue)?;
        let orphans: Vec<Id> = self
            .children(Some(issue))
            .map(|(id, _)| id.clone())
            .collect();
        for orphan in orphans {
            if let Some(child) = self.issues.get_mut(&orphan) {
                child.parent.clone_from(&removed.parent);
            }
        }
        Ok(())
    }

    /// Approve a run of the issue's agent: it starts working, acting for
    /// you.
    ///
    /// # Errors
    /// Refuses an issue with no agent assigned, one whose run is going, or
    /// an invoker who is not a person.
    #[action(audience = "people", writes = issues)]
    pub fn approve_run(&mut self, cx: &mut ActionCx, issue: &Id) -> Result<(), Refusal> {
        let starter = person(cx)?;
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let agent = target
            .assigned_agent()
            .ok_or_else(|| Refusal::new("Assign the issue to an agent first"))?
            .to_owned();
        if target.active_run().is_some() {
            return Err(Refusal::new("A run of this issue is going already"));
        }
        let epoch = target
            .run
            .as_ref()
            .map_or(0, |run| run.epoch)
            .saturating_add(1);
        let model = target.model.clone();
        target.run = Some(Run {
            epoch,
            agent: ui::tilo_resource::Atom(agent),
            starter,
            model,
            phase: RunPhase::Approved,
            started_ms: now,
            updated_ms: now,
            confirmation: None,
        });
        target.status = Status::InProgress;
        Ok(())
    }

    /// Stop the issue's run. What it already changed stays.
    ///
    /// # Errors
    /// Refuses an issue with no run going.
    #[action(audience = "people", writes = issues)]
    pub fn stop_run(&mut self, cx: &mut ActionCx, issue: &Id) -> Result<(), Refusal> {
        person(cx)?;
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let run = target
            .run
            .as_mut()
            .filter(|run| run.phase.active())
            .ok_or_else(|| Refusal::new("No run of this issue is going"))?;
        run.phase = RunPhase::Stopped;
        run.updated_ms = now;
        target.status = Status::Todo;
        Ok(())
    }

    /// Set how long the issue's runs may take; clear both to use the
    /// defaults.
    ///
    /// # Errors
    /// Refuses while a run is going, or ceilings out of range.
    #[action(audience = "people", writes = issues)]
    pub fn set_execution_budget(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The longest one turn may take, in milliseconds (up to 900000)")] turn_ms: Option<
            u64,
        >,
        #[param("The longest the run may take, in milliseconds (up to 7200000)")] total_ms: Option<
            u64,
        >,
    ) -> Result<(), Refusal> {
        person(cx)?;
        let target = self.issue_mut(issue)?;
        if target.active_run().is_some() {
            return Err(Refusal::new("Change the budget when no run is going"));
        }
        target.budget = match (turn_ms, total_ms) {
            (None, None) => None,
            (Some(turn_ms), Some(total_ms)) if Budget::admits(turn_ms, total_ms) => {
                Some(Budget { turn_ms, total_ms })
            }
            _ => {
                return Err(Refusal::new(
                    "A budget gives both ceilings, a turn no longer than the run",
                ));
            }
        };
        Ok(())
    }

    /// Send a message to the issue's run: its agent reads it at its next
    /// step.
    ///
    /// # Errors
    /// Refuses an empty message, or an issue with no run going.
    #[action(field = "comments", writes = issues)]
    pub fn send_message(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        text: String,
    ) -> Result<(), Refusal> {
        let text = message_text(&text)?;
        let author = author(cx)?;
        let agent = by_agent(cx);
        let at_ms = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let epoch = target
            .active_run()
            .map(|run| run.epoch)
            .ok_or_else(|| Refusal::new("No run of this issue is going to read it"))?;
        push_message(
            target,
            cx,
            Message {
                author,
                by_agent: agent,
                at_ms,
                text,
                kind: MessageKind::ToRun,
                run: epoch,
                incorporated: false,
            },
        );
        Ok(())
    }

    /// Report the run's phase, with what it did or found.
    ///
    /// # Errors
    /// Refuses anyone but the run's agent, an ended run, or a phase a run
    /// does not report.
    #[action(audience = "agents", writes = issues)]
    pub fn report_run(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        phase: RunPhase,
        #[param("What it did or found, in markdown")] text: Option<String>,
    ) -> Result<(), Refusal> {
        if !phase.reportable() {
            return Err(Refusal::new(
                "A run reports accepted, planning or executing",
            ));
        }
        let text = text.map(|text| message_text(&text)).transpose()?;
        let at_ms = cx.now_ms();
        let author = author(cx)?;
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        run.phase = phase;
        run.updated_ms = at_ms;
        let epoch = run.epoch;
        if let Some(text) = text {
            push_message(
                target,
                cx,
                Message {
                    author,
                    by_agent: true,
                    at_ms,
                    text,
                    kind: MessageKind::FromRun,
                    run: epoch,
                    incorporated: false,
                },
            );
        }
        Ok(())
    }

    /// Mark the first `through` messages to the run as taken in by its
    /// agent: it takes them in in order.
    ///
    /// # Errors
    /// Refuses anyone but the run's agent, or more messages than the run
    /// has.
    #[action(audience = "agents", writes = issues)]
    pub fn incorporate(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("How many of the run's messages, oldest first")] through: u64,
    ) -> Result<(), Refusal> {
        let target = self.issue_mut(issue)?;
        let epoch = own_run(cx, target)?.epoch;
        let to_run: Vec<Id> = target
            .comments
            .iter()
            .filter(|(_, message)| message.kind == MessageKind::ToRun && message.run == epoch)
            .map(|(id, _)| id.clone())
            .collect();
        let through = usize::try_from(through).unwrap_or(usize::MAX);
        if through > to_run.len() {
            return Err(Refusal::new("The run has not had that many messages"));
        }
        for id in to_run.iter().take(through) {
            if let Some(message) = target.comments.get_mut(id) {
                message.incorporated = true;
            }
        }
        Ok(())
    }

    /// Ask the run's person before an action whose answer is "ask first".
    ///
    /// # Errors
    /// Refuses anyone but the run's agent, or a run already waiting.
    #[action(audience = "agents", writes = issues)]
    pub fn ask_confirmation(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("What the run asks about")] question: Question,
    ) -> Result<(), Refusal> {
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        if run.phase == RunPhase::Waiting {
            return Err(Refusal::new("The run waits for an answer already"));
        }
        run.phase = RunPhase::Waiting;
        run.updated_ms = now;
        run.confirmation = Some(Confirmation {
            id: question.id,
            app: question.app,
            action: question.action,
            resource: question.resource,
            arguments_digest: question.arguments_digest,
            draft: question.draft,
            preview: question.preview,
            ..Confirmation::default()
        });
        Ok(())
    }

    /// Answer the run's question: confirm the action (with your edits to
    /// its draft) or decline it, saying why.
    ///
    /// # Errors
    /// Refuses when the run asks nothing, or an invoker who is not a person.
    #[action(audience = "people", writes = issues)]
    pub fn answer_confirmation(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        confirm: bool,
        #[param("Your edits to the draft's fields")] edits: Option<serde_json::Value>,
        #[param("Why you decline")] reason: Option<String>,
    ) -> Result<(), Refusal> {
        let who = person(cx)?;
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let run = target
            .run
            .as_mut()
            .filter(|run| run.phase == RunPhase::Waiting)
            .ok_or_else(|| Refusal::new("The run is not asking anything"))?;
        let question = run
            .confirmation
            .as_mut()
            .filter(|question| question.state == ConfirmationState::Waiting)
            .ok_or_else(|| Refusal::new("The run is not asking anything"))?;
        question.state = if confirm {
            ConfirmationState::Confirmed
        } else {
            ConfirmationState::Declined
        };
        question.edits = edits.unwrap_or(serde_json::Value::Null);
        question.reason = reason.unwrap_or_default();
        question.answered_by = who;
        run.phase = RunPhase::Executing;
        run.updated_ms = now;
        Ok(())
    }

    /// Settle the run's answered question once the door acted on it: a
    /// confirmation admits its one invocation, never a later one.
    ///
    /// # Errors
    /// Refuses anyone but the run's agent, or another question.
    #[action(audience = "agents", writes = issues)]
    pub fn settle_confirmation(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The question's id")] id: String,
    ) -> Result<(), Refusal> {
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        if run.confirmation.as_ref().is_none_or(|question| {
            question.id != id || question.state == ConfirmationState::Waiting
        }) {
            return Err(Refusal::new("That question is not answered"));
        }
        run.confirmation = None;
        Ok(())
    }

    /// End the run: done goes to review, failed back to Todo.
    ///
    /// # Errors
    /// Refuses anyone but the run's agent, or an ended run.
    #[action(audience = "agents", writes = issues)]
    pub fn finish_run(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        outcome: RunOutcome,
        #[param("What it did, in markdown")] summary: Option<String>,
    ) -> Result<(), Refusal> {
        let summary = summary.map(|text| message_text(&text)).transpose()?;
        let at_ms = cx.now_ms();
        let author = author(cx)?;
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        run.phase = match outcome {
            RunOutcome::Done => RunPhase::Done,
            RunOutcome::Failed => RunPhase::Failed,
            RunOutcome::Stopped => RunPhase::Stopped,
        };
        run.updated_ms = at_ms;
        let epoch = run.epoch;
        target.status = match outcome {
            RunOutcome::Done => Status::InReview,
            RunOutcome::Failed | RunOutcome::Stopped => Status::Todo,
        };
        if let Some(text) = summary {
            push_message(
                target,
                cx,
                Message {
                    author,
                    by_agent: true,
                    at_ms,
                    text,
                    kind: MessageKind::FromRun,
                    run: epoch,
                    incorporated: false,
                },
            );
        }
        Ok(())
    }

    /// Import tasks the workspace's room kept before the tracker. A task
    /// already imported is left alone.
    #[action(housekeeping, writes = issues)]
    pub fn import(&mut self, tasks: Vec<Imported>) {
        for task in tasks {
            if self.imported.contains(&task.task) {
                continue;
            }
            let id = task.issue_id();
            if !self.issues.contains(&id) {
                let number = self.next_number();
                self.issues.push(id, task.issue(number));
            }
            self.imported.insert(task.task.clone());
        }
    }
}
