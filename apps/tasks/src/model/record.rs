//! The tracker: every issue of the workspace, and the verbs on them.

use tilo_rich_text::RichText;
use ui::tilo_actions::ActorKind;
use ui::tilo_reference::{ProgramStep, ResourceAt};
use ui::tilo_resource::{
    self, ActionCx, Collection, Counter, Id, Model, NonBlank, Refusal, Set, resource,
};

use crate::model::issue::{
    AgentRequest, Imported, Issue, Link, MAX_MESSAGE_CHARS, MAX_READ_RESOURCE_CHARS, Message,
    MessageKind, Status,
};
use crate::model::run::{
    Area, Budget, Confirmation, ConfirmationState, MAX_TOTAL_MS, MAX_TURN_MS, ModelChoice,
    PausedRun, Question, Run, RunOutcome, RunPhase, Spend, SpendLimits, StepSpend, money,
};
use crate::model::tracker_area::{Kind, Person, TrackerArea};

/// A workspace's issues, the projects and areas above them, and who is
/// involved in each.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Tracker {
    /// Issues
    ///
    /// Flat, in the order the board shows them: a sub-issue names its
    /// parent. Each issue is a storage unit of its own.
    #[model(unit)]
    pub issues: Collection<Issue>,
    /// Projects
    ///
    /// The projects and areas above the issues, each an ordinary resource
    /// of its own. Nothing else is kept here on their behalf: what a
    /// project holds is the issues that work towards it and the links those
    /// carry.
    #[model(unit)]
    pub areas: Collection<TrackerArea>,
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

    /// The issue and every issue under it, each with the last run's ledger
    /// folded in.
    fn subtree_spend(&self, issue: &Id) -> Spend {
        let mut total = Spend::default();
        for (id, child) in self.issues.iter() {
            if !self.is_under(id, issue) {
                continue;
            }
            let Some(run) = child.run.as_ref().and_then(|run| run.spend.as_ref()) else {
                continue;
            };
            total.steps_taken = total.steps_taken.saturating_add(run.steps_taken);
            total.input_tokens = total.input_tokens.saturating_add(run.input_tokens);
            total.output_tokens = total.output_tokens.saturating_add(run.output_tokens);
            total.priced_calls = total.priced_calls.saturating_add(run.priced_calls);
            total.cost_micros = total.cost_micros.saturating_add(run.cost_micros);
            total.held_tokens = total.held_tokens.saturating_add(run.held_tokens);
            total.held_cost_micros = total.held_cost_micros.saturating_add(run.held_cost_micros);
            if total.currency.is_empty() {
                total.currency.clone_from(&run.currency);
            }
        }
        total
    }

    /// What the issue and its sub-issues have spent, held steps included.
    fn spent_by(&self, issue: &Id) -> Spend {
        self.subtree_spend(issue)
    }

    /// Every spend limit over the issue: its own and each ancestor's, which
    /// its work draws from.
    fn limits_over(&self, issue: &Id) -> Vec<Budget> {
        let mut limits = Vec::new();
        let mut at = Some(issue.clone());
        for _ in 0..=self.issues.len() {
            let Some(id) = at.clone() else { break };
            let Some(found) = self.issues.get(&id) else {
                break;
            };
            if let Some(budget) = found.budget.as_ref()
                && !budget.spends_freely()
            {
                limits.push(budget.clone());
            }
            at.clone_from(&found.parent);
        }
        limits
    }

    /// Write the issue's money total, and every ancestor's, from the ledger:
    /// a subtask's spend counts towards its parent's, to the top.
    fn roll_up_spend(&mut self, issue: &Id) {
        let mut at = Some(issue.clone());
        for _ in 0..=self.issues.len() {
            let Some(id) = at.clone() else { break };
            let total = self.subtree_spend(&id);
            let Some(found) = self.issues.get_mut(&id) else {
                break;
            };
            found.spend = (total.cost_micros > 0).then_some(total.cost_micros);
            at.clone_from(&found.parent);
        }
    }

    fn no_area() -> Refusal {
        Refusal::new("This project no longer exists")
    }

    fn area_mut(&mut self, area: &Id) -> Result<&mut TrackerArea, Refusal> {
        self.areas.get_mut(area).ok_or_else(Self::no_area)
    }

    /// The project or area named `area`, when it is one of the tracker's.
    #[must_use]
    pub fn area(&self, area: &Id) -> Option<&TrackerArea> {
        self.areas.get(area)
    }

    /// The issues that work towards `area`, in board order: the tasks the
    /// project holds. Each works towards it through its own
    /// [`area`](Issue::area), so it reads in the project and in its own
    /// list alike.
    pub fn in_area<'a>(&'a self, area: &'a Id) -> impl Iterator<Item = (&'a Id, &'a Issue)> {
        self.issues.iter().filter(move |(_, issue)| {
            issue.area.as_ref().is_some_and(|named| {
                named.kind == crate::model::KIND && named.resource == area.as_str()
            })
        })
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

fn message_text(text: NonBlank) -> Result<String, Refusal> {
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(Refusal::new("This message is too long"));
    }
    Ok(text.into_string())
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

/// Refuse a step of a run a person paused: it takes none until they let it
/// go on.
fn at_boundary(run: &Run) -> Result<(), Refusal> {
    if run.paused.is_some() {
        return Err(Refusal::new("The run is paused: no step until it goes on"));
    }
    Ok(())
}

/// Why the step `next`, after `spent`, crosses one of `limits`; `None` when
/// every limit admits it. The reason names the limit it crosses.
fn over(limits: &[Budget], spent: &Spend, next: &StepSpend) -> Option<String> {
    let mut after = spent.clone();
    after.held_tokens = after.held_tokens.saturating_add(next.tokens());
    after.held_cost_micros = after.held_cost_micros.saturating_add(next.cost_micros);
    after.priced_calls = after.priced_calls.saturating_add(next.priced_calls);
    limits.iter().find_map(|limit| {
        let reached = limit.reached(&after);
        if reached.is_some() {
            return reached;
        }
        let tokens_over = limit.max_tokens.is_some_and(|max| after.tokens() > max);
        if tokens_over {
            return Some(format!(
                "The step would take this task past its budget of {} tokens",
                limit.max_tokens.unwrap_or_default()
            ));
        }
        let calls_over = limit
            .max_priced_calls
            .is_some_and(|max| after.priced_calls > max);
        if calls_over {
            return Some(format!(
                "The step would take this task past its budget of {} priced calls",
                limit.max_priced_calls.unwrap_or_default()
            ));
        }
        let cost_over = limit.max_cost_micros.is_some_and(|max| after.cost() > max);
        cost_over.then(|| {
            let named = limit.currency.trim();
            format!(
                "The step would take this task past its budget of {} {named}",
                money(limit.max_cost_micros.unwrap_or_default())
            )
            .replace("  ", " ")
        })
    })
}

/// Start the next run of `issue`'s assigned agent, acting for `starter`.
/// Answers the run's epoch.
fn start_run(issue: &mut Issue, starter: String, now: i64) -> Result<u64, Refusal> {
    let agent = issue
        .assigned_agent()
        .ok_or_else(|| Refusal::new("Assign the issue to an agent first"))?
        .to_owned();
    if issue.active_run().is_some() {
        return Err(Refusal::new("A run of this issue is going already"));
    }
    let epoch = issue
        .run
        .as_ref()
        .map_or(0, |run| run.epoch)
        .saturating_add(1);
    let model = issue.model.clone();
    // A task agent's delegation is for its task (this issue) and the person
    // it acts for: the issue's next run by the same agent for the same
    // person carries it, so a conversation goes on with the agent made for
    // it. Anyone else starts a run its delegation does not cover.
    let delegation = issue
        .run
        .as_ref()
        .filter(|previous| previous.agent.0 == agent && previous.starter == starter)
        .and_then(|previous| previous.delegation.clone());
    issue.run = Some(Run {
        epoch,
        agent: ui::tilo_resource::Atom(agent),
        starter,
        delegation,
        model,
        device: None,
        phase: RunPhase::Approved,
        started_ms: now,
        updated_ms: now,
        confirmation: None,
        paused: None,
        spend: None,
    });
    issue.status = Status::InProgress;
    Ok(epoch)
}

/// A person who gives `issue` to an agent starts its run then, acting for
/// them, as their own ask does: no separate approval. The run is admitted
/// step by step against that person's live grants like any run, so it never
/// does what they could not, and a step that asks first waits for its
/// answer. Work an agent or an automation hands on waits for a person's
/// `approve-run`, there being no person whose authority it would act with.
fn run_when_a_person_assigns(issue: &mut Issue, cx: &ActionCx, now: i64) -> Result<(), Refusal> {
    if issue.assigned_agent().is_none() || by_agent(cx) {
        return Ok(());
    }
    let Ok(starter) = person(cx) else {
        return Ok(());
    };
    start_run(issue, starter, now).map(drop)
}

fn push_message(issue: &mut Issue, cx: &mut ActionCx, message: Message) {
    let id = cx.id();
    issue.comments.push(id, message);
}

#[resource(
    kind = "tasks.tracker",
    label = "Tasks",
    params = tracker,
    search = crate::model::search::entries,
    effects = crate::note::TasksEffect
)]
impl Tracker {
    /// Create an issue.
    ///
    /// # Errors
    /// Refuses a parent that no longer exists, or an invalid assignee.
    #[action(writes = issues)]
    pub fn create(
        &mut self,
        cx: &mut ActionCx,
        #[param("What the issue is about, in a line")] title: NonBlank,
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
            title: title.into_string(),
            body: RichText::from_markdown(body.unwrap_or_default()),
            status: status.unwrap_or_default(),
            assignee: assignee.map(ui::tilo_resource::Atom),
            parent,
            author,
            created_ms: cx.now_ms(),
            ..Issue::default()
        };
        let id = cx.id();
        self.issues.push(id.clone(), issue);
        let now = cx.now_ms();
        run_when_a_person_assigns(self.issue_mut(&id)?, cx, now)
    }

    /// Create a project or an area: the resource above the issues written
    /// for it. It starts with nobody involved and holds nothing of its own
    /// yet; tasks join it by working towards it.
    ///
    /// # Errors
    /// Refuses a name too long, or an id already taken.
    #[action(writes = areas)]
    pub fn create_area(
        &mut self,
        cx: &mut ActionCx,
        #[param("Its name")] name: NonBlank,
        #[param("Whether it is a project with an end or an area that goes on")] kind: Option<Kind>,
    ) -> Result<(), Refusal> {
        let id = cx.id();
        if self.areas.contains(&id) {
            return Err(Refusal::new("That project exists already"));
        }
        let area = TrackerArea {
            id: id.clone(),
            name: name.into_string(),
            kind: kind.unwrap_or_default(),
            ..TrackerArea::default()
        };
        TrackerArea::valid(&area)?;
        self.areas.push(id, area);
        Ok(())
    }

    /// Rename the project or area `area`.
    ///
    /// # Errors
    /// Refuses a project that no longer exists, or a name too long.
    #[action(item = areas, writes = areas)]
    pub fn rename_area(&mut self, area: &Id, name: NonBlank) -> Result<(), Refusal> {
        let target = self.area_mut(area)?;
        if name.chars().count() > crate::model::tracker_area::MAX_NAME_CHARS {
            return Err(Refusal::new("A project's name fits on a line"));
        }
        target.name = name.into_string();
        Ok(())
    }

    /// Say whether the project or area `area` reaches an end or goes on: a
    /// project with a goal, or an area that is a standing responsibility.
    ///
    /// # Errors
    /// Refuses a project that no longer exists.
    #[action(item = areas, writes = areas)]
    pub fn set_kind(
        &mut self,
        area: &Id,
        #[param("Project or area")] kind: Kind,
    ) -> Result<(), Refusal> {
        self.area_mut(area)?.kind = kind;
        Ok(())
    }

    /// Say who is involved in the project or area `area`, and what they do
    /// there. Naming someone grants them nothing: who may read the project
    /// is the sharing it was given, never a list of people.
    ///
    /// # Errors
    /// Refuses a project that no longer exists.
    #[action(item = areas, writes = areas)]
    pub fn involve(
        &mut self,
        cx: &mut ActionCx,
        area: &Id,
        #[param("The person's name, as the workspace reads it")] name: NonBlank,
        #[param("What they do here, in their own words")] role: Option<String>,
    ) -> Result<(), Refusal> {
        let id = cx.id();
        let target = self.area_mut(area)?;
        target.people.push(
            id,
            Person {
                name: name.into_string(),
                role: role.unwrap_or_default().trim().to_owned(),
            },
        );
        Ok(())
    }

    /// Take someone out of the list of people involved in `area`. They keep
    /// their access to it: that is the project's sharing, not this list.
    ///
    /// # Errors
    /// Refuses a project that no longer exists.
    #[action(item = areas, writes = areas)]
    pub fn remove_person(&mut self, area: &Id, person: &Id) -> Result<(), Refusal> {
        self.area_mut(area)?.people.remove(person);
        Ok(())
    }

    /// Move an issue under another one, or to the top level.
    ///
    /// # Errors
    /// Refuses moving an issue under itself or one of its own sub-issues.
    #[action(item = issues, writes = issues)]
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

    /// Assign an issue to a person or an agent, or to nobody. A person
    /// assigning it to an agent starts its run at once, acting for them.
    ///
    /// # Errors
    /// Refuses while an agent's run of the issue is going, or an assignee
    /// that is neither `person:<id>` nor `agent:<id>`.
    #[action(item = issues, writes = issues)]
    pub fn assign(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("person:<id> or agent:<id>; nobody without")] assignee: Option<String>,
    ) -> Result<(), Refusal> {
        let now = cx.now_ms();
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
        run_when_a_person_assigns(target, cx, now)
    }

    /// Choose the model the issue's agent plans with, or let it plan with
    /// the one chosen for the agent.
    ///
    /// # Errors
    /// Refuses while a run of the issue is going, or a choice that names no
    /// provider or model.
    #[action(item = issues, writes = issues)]
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
            quick: ask.quick,
            author: starter.clone(),
            created_ms: now,
            // No run has a cap of its own: one the person asks goes until
            // it is done or stopped, within the limits they set.
            budget: None,
            run: Some(Run {
                epoch: 1,
                agent,
                starter,
                delegation: ask.delegation,
                phase: RunPhase::Approved,
                model: ask.model,
                device: None,
                started_ms: now,
                updated_ms: now,
                confirmation: None,
                paused: None,
                spend: None,
            }),
            ..Issue::default()
        };
        self.issues.push(ask.id, issue);
        Ok(())
    }

    /// Label an issue.
    ///
    /// # Errors
    /// Refuses an issue at its 32 labels.
    #[action(item = issues, writes = issues)]
    pub fn add_label(&mut self, issue: &Id, label: NonBlank) -> Result<(), Refusal> {
        self.issue_mut(issue)?.labels.insert(label.into_string());
        Ok(())
    }

    /// Place an issue on the board: in a status column, before another
    /// issue or at the end.
    ///
    /// # Errors
    /// Refuses an issue that no longer exists.
    #[action(item = issues, writes = issues)]
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
    /// Refuses a message too long.
    #[action(item = issues, field = "comments", writes = issues)]
    pub fn comment(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        text: NonBlank,
    ) -> Result<(), Refusal> {
        let text = message_text(text)?;
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
    /// Refuses an issue that no longer exists.
    #[action(item = issues, writes = issues)]
    pub fn link(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The app that keeps the resource")] app: NonBlank,
        #[param("The resource's kind")] kind: String,
        #[param("The resource's address in its app")] resource: NonBlank,
        #[param("How the link reads")] label: String,
        #[param("The app's record that keeps it; its record of the workspace without")]
        record: Option<String>,
    ) -> Result<(), Refusal> {
        let id = cx.id();
        self.issue_mut(issue)?.links.push(
            id,
            Link {
                label,
                app: app.into_string(),
                kind,
                resource: resource.into_string(),
                record: record.filter(|record| !record.is_empty()),
            },
        );
        Ok(())
    }

    /// Link a note
    ///
    /// Link an issue to an existing note in Notes, picked by its title.
    ///
    /// # Errors
    /// Refuses a reference that names no note.
    #[action(item = issues, writes = issues)]
    pub fn link_note(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The note", resource = "notes.notebook.pages")] note: ResourceAt,
    ) -> Result<(), Refusal> {
        let (Some(app), Some(resource)) = (
            NonBlank::parse(&note.app),
            NonBlank::parse(&note.reference.id),
        ) else {
            return Err(Refusal::new("That is not a note"));
        };
        if note.app != crate::NOTES || note.reference.kind != crate::note::NOTE_KIND {
            return Err(Refusal::new("That is not a note"));
        }
        self.link(
            cx,
            issue,
            app,
            note.reference.kind,
            resource,
            note.reference.label,
            note.record,
        )
    }

    /// Delete an issue. Its sub-issues move up to its parent.
    ///
    /// # Errors
    /// Refuses an issue that no longer exists.
    #[action(item = issues, destructive, writes = issues)]
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
    #[action(item = issues, audience = "people", writes = issues)]
    pub fn approve_run(&mut self, cx: &mut ActionCx, issue: &Id) -> Result<(), Refusal> {
        let starter = person(cx)?;
        let now = cx.now_ms();
        start_run(self.issue_mut(issue)?, starter, now).map(drop)
    }

    /// Tell the issue's agent something: the run going reads it at its next
    /// step, and with none going, the next run starts, approved by you, and
    /// reads it as its direction. It is how a conversation with an agent
    /// goes on after a run ended.
    ///
    /// # Errors
    /// Refuses an issue with no agent assigned, a message too long, or an
    /// invoker who is not a person.
    #[action(item = issues, audience = "people", writes = issues)]
    pub fn tell_agent(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        text: NonBlank,
    ) -> Result<(), Refusal> {
        let text = message_text(text)?;
        let author = person(cx)?;
        let at_ms = cx.now_ms();
        let target = self.issue_mut(issue)?;
        if target.assigned_agent().is_none() {
            return Err(Refusal::new("Assign the issue to an agent first"));
        }
        // The message belongs to the run that will read it: the one going,
        // or the one it starts, so the broker hands it to that run as one
        // of its directions.
        let run = match target.active_run() {
            Some(run) => run.epoch,
            None => start_run(target, author.clone(), at_ms)?,
        };
        push_message(
            target,
            cx,
            Message {
                author,
                by_agent: false,
                at_ms,
                text,
                kind: MessageKind::ToRun,
                run,
                incorporated: false,
                ..Message::default()
            },
        );
        Ok(())
    }

    /// Pause the issue's run: it takes no new step until a person lets it
    /// go on. The step it was in, and the message boundary it reached, are
    /// recorded so that resuming continues from there. What it already
    /// changed stays.
    ///
    /// # Errors
    /// Refuses an issue with no run going, or an invoker who is not a
    /// person.
    #[action(item = issues, audience = "people", writes = issues)]
    pub fn pause_run(&mut self, cx: &mut ActionCx, issue: &Id) -> Result<(), Refusal> {
        person(cx)?;
        let now = cx.now_ms();
        // The boundary is the messages to this run that exist when the
        // person pauses it: it goes on after them and no further, so taking
        // one in again never repeats a step it already planned on.
        let target = self.issue_mut(issue)?;
        let (epoch, phase) = target
            .run
            .as_ref()
            .filter(|run| run.phase.active())
            .map(|run| (run.epoch, run.phase))
            .ok_or_else(|| Refusal::new("No run of this issue is going"))?;
        if target.run.as_ref().is_some_and(|run| run.paused.is_some()) {
            return Err(Refusal::new("The run is paused already"));
        }
        let through = target
            .comments
            .values()
            .filter(|message| message.kind == MessageKind::ToRun && message.run == epoch)
            .count();
        let paused = PausedRun::of(phase, u64::try_from(through).unwrap_or(u64::MAX), now)
            .ok_or_else(|| Refusal::new("This step is not one a pause can hold"))?;
        let run = target
            .run
            .as_mut()
            .ok_or_else(|| Refusal::new("No run of this issue is going"))?;
        // Only the boundary is written: the run stays in the step it was in,
        // so a step its agent reports meanwhile merges rather than being
        // overwritten by a step a person never moved it out of.
        run.paused = Some(paused.because(""));
        Ok(())
    }

    /// Pause the issue's run because it reached a budget: it waits in the
    /// step it was in, for a person to raise the limit or let it go on, and
    /// `reason` says which limit it reached.
    ///
    /// # Errors
    /// Refuses anyone but the run's own agent, or an ended or paused run.
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn pause_run_for_budget(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("Why it stopped: the limit it reached")] reason: NonBlank,
    ) -> Result<(), Refusal> {
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let epoch = own_run(cx, target)?.epoch;
        if target.run.as_ref().is_some_and(|run| run.paused.is_some()) {
            return Err(Refusal::new("The run is paused already"));
        }
        let through = target
            .comments
            .values()
            .filter(|message| message.kind == MessageKind::ToRun && message.run == epoch)
            .count();
        let paused = PausedRun::of(
            RunPhase::Executing,
            u64::try_from(through).unwrap_or(u64::MAX),
            now,
        )
        .ok_or_else(|| Refusal::new("This step is not one a pause can hold"))?
        .because(reason.as_str());
        let run = target
            .run
            .as_mut()
            .ok_or_else(|| Refusal::new("No run of this issue is going"))?;
        run.paused = Some(paused);
        Ok(())
    }

    /// Let the issue's paused run go on from where it stopped.
    ///
    /// # Errors
    /// Refuses an issue whose run is not paused, or an invoker who is not a
    /// person.
    #[action(item = issues, audience = "people", writes = issues)]
    pub fn resume_run(&mut self, cx: &mut ActionCx, issue: &Id) -> Result<(), Refusal> {
        person(cx)?;
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let run = target
            .run
            .as_mut()
            .filter(|run| run.phase.active())
            .ok_or_else(|| Refusal::new("No run of this issue is going"))?;
        let paused = run
            .paused
            .take()
            .ok_or_else(|| Refusal::new("No run of this issue is paused"))?;
        let phase = paused
            .phase()
            .ok_or_else(|| Refusal::new("The run does not name the step it stopped in"))?;
        run.phase = phase;
        run.updated_ms = now;
        Ok(())
    }

    /// Stop the issue's run. What it already changed stays.
    ///
    /// # Errors
    /// Refuses an issue with no run going.
    #[action(item = issues, audience = "people", writes = issues)]
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
        run.paused = None;
        run.updated_ms = now;
        target.status = Status::Todo;
        Ok(())
    }

    /// End a run that stopped showing a step longer than `after_ms` ago,
    /// as idle: what it did stays, and the issue is left to be picked up
    /// again.
    ///
    /// Any change of the run moves when it last showed one, so a run that
    /// keeps reporting its step is never ended this way, and a paused one
    /// waits as long as its person leaves it.
    ///
    /// # Errors
    /// Refuses a run that is not going, or one that showed a step more
    /// recently than `after_ms`.
    #[action(item = issues, housekeeping, writes = issues)]
    pub fn end_idle_run(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("How long the run may go without a step, in milliseconds")] after_ms: i64,
    ) -> Result<(), Refusal> {
        if after_ms <= 0 {
            return Err(Refusal::new("An idle run waits at least a moment"));
        }
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let run = target
            .run
            .as_mut()
            .filter(|run| run.phase.active() && run.paused.is_none())
            .ok_or_else(|| Refusal::new("No run of this issue is going"))?;
        if now.saturating_sub(run.updated_ms) < after_ms {
            return Err(Refusal::new("The run showed a step that recently"));
        }
        run.phase = RunPhase::Stopped;
        run.updated_ms = now;
        target.status = Status::Todo;
        Ok(())
    }

    /// Set how long the issue's runs may take and what they may spend;
    /// clear every ceiling to use the defaults.
    ///
    /// # Errors
    /// Refuses while a run is going, time ceilings out of range, or a spend
    /// limit no run can work under.
    #[action(item = issues, audience = "people", writes = issues)]
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
        #[param("The most its runs may spend: tokens, priced calls and money")] limits: Option<
            SpendLimits,
        >,
        #[param("The most model turns one run may take (up to 10000)")] max_turns: Option<u64>,
    ) -> Result<(), Refusal> {
        person(cx)?;
        let target = self.issue_mut(issue)?;
        if target.active_run().is_some() {
            return Err(Refusal::new("Change the budget when no run is going"));
        }
        if !Budget::admits_turns(max_turns) {
            return Err(Refusal::new(
                "A run's turns are at least one and at most 10000",
            ));
        }
        let ceilings = match (turn_ms, total_ms) {
            (None, None) => None,
            (Some(turn_ms), Some(total_ms)) if Budget::admits(turn_ms, total_ms) => {
                Some((turn_ms, total_ms))
            }
            _ => {
                return Err(Refusal::new(
                    "A budget gives both time ceilings, a turn no longer than the run",
                ));
            }
        };
        let limits = limits.map(SpendLimits::checked).transpose()?;
        target.budget = if ceilings.is_none()
            && max_turns.is_none()
            && limits.as_ref().is_none_or(SpendLimits::is_unlimited)
        {
            None
        } else {
            let (turn_ms, total_ms) = ceilings.unwrap_or((MAX_TURN_MS, MAX_TOTAL_MS));
            let limits = limits.unwrap_or_default();
            Some(Budget {
                turn_ms,
                total_ms,
                max_turns,
                max_tokens: limits.max_tokens,
                max_priced_calls: limits.max_priced_calls,
                max_cost_micros: limits.max_cost_micros,
                currency: limits.currency,
            })
        };
        Ok(())
    }

    /// Plan the issue: the goal it works towards, how urgent it is and what
    /// its work may cost. Each argument is optional, and the one given is the
    /// one written; the others stay as they were.
    ///
    /// # Errors
    /// Refuses a goal that is not a resource, an urgency outside 1 to 4, or
    /// a budget of nothing.
    #[action(item = issues, audience = "people", writes = issues)]
    pub fn plan(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The resource it works towards: resource, app, kind and label")] area: Option<Area>,
        #[param("How urgent it is, 1 (most urgent) to 4")] priority: Option<u8>,
        #[param("The most its work may cost, in the workspace's units")] cost_budget: Option<u64>,
        #[param("When it is wanted, in milliseconds since the epoch")] due_ms: Option<i64>,
    ) -> Result<(), Refusal> {
        person(cx)?;
        let target = self.issue_mut(issue)?;
        if let Some(area) = area {
            if !area.names_a_resource() {
                return Err(Refusal::new("A goal is a resource of the workspace"));
            }
            target.area = Some(area);
        }
        if let Some(priority) = priority {
            if !(1..=4).contains(&priority) {
                return Err(Refusal::new("A priority is 1 (most urgent) to 4"));
            }
            target.priority = Some(priority);
        }
        if let Some(cost_budget) = cost_budget {
            if cost_budget == 0 {
                return Err(Refusal::new("A cost budget is more than nothing"));
            }
            target.cost_budget = Some(cost_budget);
        }
        if let Some(due_ms) = due_ms {
            target.due_ms = Some(due_ms);
        }
        Ok(())
    }

    /// Send a message to the issue's run: its agent reads it at its next
    /// step. Without a run going, the message joins the thread instead, so
    /// it stays with the issue and every collaborator reads it there.
    ///
    /// # Errors
    /// Refuses a message too long.
    #[action(item = issues, field = "comments", writes = issues)]
    pub fn send_message(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        text: NonBlank,
    ) -> Result<(), Refusal> {
        let text = message_text(text)?;
        let author = author(cx)?;
        let agent = by_agent(cx);
        let at_ms = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let (kind, run, incorporated) = match target.active_run() {
            Some(run) => (MessageKind::ToRun, run.epoch, false),
            None => (MessageKind::Comment, 0, true),
        };
        push_message(
            target,
            cx,
            Message {
                author,
                by_agent: agent,
                at_ms,
                text,
                kind,
                run,
                incorporated,
                ..Message::default()
            },
        );
        Ok(())
    }

    /// Report the run's phase, with what it did or found.
    ///
    /// # Errors
    /// Refuses anyone but the run's agent, an ended run, or a phase a run
    /// does not report.
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn report_run(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        phase: RunPhase,
        #[param("What it did or found, in markdown")] text: Option<NonBlank>,
    ) -> Result<(), Refusal> {
        if !phase.reportable() {
            return Err(Refusal::new(
                "A run reports accepted, planning or executing",
            ));
        }
        let text = text.map(message_text).transpose()?;
        let at_ms = cx.now_ms();
        let author = author(cx)?;
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        at_boundary(run)?;
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
                    ..Message::default()
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
    #[action(item = issues, audience = "agents", writes = issues)]
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
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn ask_confirmation(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("What the run asks about")] question: Question,
    ) -> Result<(), Refusal> {
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        at_boundary(run)?;
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
    #[action(item = issues, audience = "people", writes = issues)]
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
    #[action(item = issues, audience = "agents", writes = issues)]
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
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn finish_run(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        outcome: RunOutcome,
        #[param("What it did, in markdown")] summary: Option<NonBlank>,
    ) -> Result<(), Refusal> {
        let summary = summary.map(message_text).transpose()?;
        let at_ms = cx.now_ms();
        let author = author(cx)?;
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        at_boundary(run)?;
        run.phase = match outcome {
            RunOutcome::Done => RunPhase::Done,
            RunOutcome::Failed => RunPhase::Failed,
            RunOutcome::Stopped => RunPhase::Stopped,
        };
        run.paused = None;
        run.updated_ms = at_ms;
        let epoch = run.epoch;
        let quick = target.quick;
        target.status = match outcome {
            // A quick question ends when it is answered: there is nothing
            // for a person to review in what the agent said back.
            RunOutcome::Done if quick => Status::Done,
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
                    ..Message::default()
                },
            );
        }
        Ok(())
    }

    /// Record the device the run is working on, as its agent says, and whose
    /// device that is: a device a run is moved to is handed over from this
    /// one, and the record keeps the last on each.
    ///
    /// # Errors
    /// Refuses anyone but the run's own agent, or an ended run.
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn run_on_device(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The device it runs on")] device: NonBlank,
    ) -> Result<(), Refusal> {
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        run.device = Some(device.into_string());
        Ok(())
    }

    /// Hold what the next step is expected to cost, until the step settles.
    /// A hold is measured against the issue's budget, so a step that would
    /// take it past a limit never starts: the refusal names the limit.
    ///
    /// # Errors
    /// Refuses anyone but the run's own agent, an ended run, a step whose
    /// hold is still out, or a hold that would take the issue's budget, or
    /// its parents', past a limit.
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn hold_spend(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The tokens the step is expected to use")] tokens: u64,
        #[param("The money the step is expected to cost, in millionths")] cost_micros: u64,
    ) -> Result<(), Refusal> {
        if tokens == 0 && cost_micros == 0 {
            return Ok(());
        }
        let spent_so_far = self.spent_by(issue);
        let limits = self.limits_over(issue);
        let next = StepSpend {
            input_tokens: tokens,
            cost_micros,
            ..StepSpend::default()
        };
        let target = self.issue_mut(issue)?;
        let epoch = own_run(cx, target)?.epoch;
        if let Some(reason) = over(&limits, &spent_so_far, &next) {
            return Err(Refusal::new(reason));
        }
        if let Some(run) = target.run.as_mut().filter(|run| run.epoch == epoch) {
            run.spend
                .get_or_insert_with(Spend::default)
                .hold(tokens, cost_micros)?;
        }
        Ok(())
    }

    /// Settle the step in flight with the receipt its provider gave: the
    /// model that answered it, the device it ran on, the tokens and calls it
    /// spent and what was priced. A step whose provider answered with an
    /// error settles at nothing.
    ///
    /// # Errors
    /// Refuses anyone but the run's own agent, an ended run, a receipt in
    /// another currency than the run's, or one that takes the issue's budget
    /// past a limit.
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn settle_spend(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The step's receipt")] step: StepSpend,
    ) -> Result<(), Refusal> {
        let spent_so_far = self.spent_by(issue);
        let limits = self.limits_over(issue);
        let target = self.issue_mut(issue)?;
        let epoch = own_run(cx, target)?.epoch;
        if let Some(reason) = over(&limits, &spent_so_far, &step) {
            return Err(Refusal::new(reason));
        }
        if let Some(run) = target.run.as_mut().filter(|run| run.epoch == epoch) {
            run.spend.get_or_insert_with(Spend::default).settle(&step)?;
        }
        self.roll_up_spend(issue);
        Ok(())
    }

    /// Record what the run read: a resource it looked up through system
    /// search, as the hit it took reads. The task lists it, so its person
    /// sees every resource the run pulled in after the one the request named,
    /// and the person who keeps that resource sees a run reach it.
    ///
    /// Reading it is not a permission: the resource was already read through
    /// the search under the run's own grants, and a hit the run may not read
    /// is never found.
    ///
    /// # Errors
    /// Refuses anyone but the run's own agent, or a read of nothing.
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn record_read(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The resource it read")] resource: ResourceAt,
    ) -> Result<(), Refusal> {
        if resource.reference.id.trim().is_empty()
            || resource.reference.id.chars().count() > MAX_READ_RESOURCE_CHARS
            || resource.app.trim().is_empty()
        {
            return Err(Refusal::new("A read names one resource it may reach"));
        }
        let label = NonBlank::parse(&resource.reference.label)
            .ok_or_else(|| Refusal::new("A read says what it read"))
            .and_then(message_text)?;
        let reference = serde_json::to_string(&resource)
            .map_err(|_| Refusal::new("A read names one resource it may reach"))?;
        let at_ms = cx.now_ms();
        let author = author(cx)?;
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        at_boundary(run)?;
        let epoch = run.epoch;
        push_message(
            target,
            cx,
            Message {
                author,
                by_agent: true,
                at_ms,
                // The resource's own name: the history entry says what kind of
                // step this was, and the thread row reads as the lookup it is.
                text: label,
                kind: MessageKind::Lookup,
                lookup: reference,
                run: epoch,
                ..Message::default()
            },
        );
        Ok(())
    }

    /// Record a step the run took: the tilolang program it ran and what it
    /// answered. The task's thread shows it as the code it was with its
    /// answer, so its person follows the run step by step and sees what each
    /// step read and changed.
    ///
    /// Recording a step is not a permission: the program already ran under
    /// the run's own grants, and what it changed is in the apps' histories.
    ///
    /// # Errors
    /// Refuses anyone but the run's own agent, or a step without a program.
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn record_step(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The step: its program and what it answered")] step: ProgramStep,
    ) -> Result<(), Refusal> {
        let step = step.bounded();
        let program = NonBlank::parse(&step.program)
            .ok_or_else(|| Refusal::new("A step names the program it ran"))
            .and_then(message_text)?;
        let recorded = serde_json::to_string(&step)
            .map_err(|_| Refusal::new("A step names the program it ran"))?;
        let at_ms = cx.now_ms();
        let author = author(cx)?;
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        at_boundary(run)?;
        let epoch = run.epoch;
        push_message(
            target,
            cx,
            Message {
                author,
                by_agent: true,
                at_ms,
                text: program,
                kind: MessageKind::Step,
                step: recorded,
                run: epoch,
                ..Message::default()
            },
        );
        Ok(())
    }

    /// Record the run's answer as a view: the resource that keeps it, which
    /// opens as a window beside what the request was about.
    ///
    /// # Errors
    /// Refuses anyone but the run's own agent, or an answer that names no
    /// resource.
    #[action(item = issues, audience = "agents", writes = issues)]
    pub fn record_answer(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The resource that keeps the answer's view")] view: ResourceAt,
    ) -> Result<(), Refusal> {
        if view.reference.id.trim().is_empty()
            || view.reference.id.chars().count() > MAX_READ_RESOURCE_CHARS
            || view.app.trim().is_empty()
        {
            return Err(Refusal::new("An answer names the resource that keeps it"));
        }
        let title = NonBlank::parse(&view.reference.label)
            .ok_or_else(|| Refusal::new("An answer names what it shows"))
            .and_then(message_text)?;
        let reference = serde_json::to_string(&view)
            .map_err(|_| Refusal::new("An answer names the resource that keeps it"))?;
        let at_ms = cx.now_ms();
        let author = author(cx)?;
        let target = self.issue_mut(issue)?;
        let run = own_run(cx, target)?;
        at_boundary(run)?;
        let epoch = run.epoch;
        push_message(
            target,
            cx,
            Message {
                author,
                by_agent: true,
                at_ms,
                text: title,
                kind: MessageKind::Answer,
                lookup: reference,
                run: epoch,
                ..Message::default()
            },
        );
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
