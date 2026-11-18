//! The tracker's issues: a collection the framework keeps beside the
//! tracker, which decides where each issue is stored, so that a change to
//! one issue of thousands costs that issue. The tracker's own document
//! ([`crate::model::Tracker`]) keeps the projects and areas above them.
//!
//! - **Numbers.** Each part of issues counts the issues made in it on top
//!   of the highest number given before it started ([`IssueList::before`]),
//!   which the framework carries over as it starts the next one
//!   ([`ui::framework::Chained::carry`]).
//! - **Order.** Each issue carries its place in the tracker's order
//!   ([`Issue::rank`]), set where it is made or placed from every issue held
//!   there ([`Issues`]), so the list and the board read across them all.
//! - **What reads across the collection.** A sub-issue whose parent is gone
//!   stands at the top level, and an issue whose project was deleted works
//!   towards none: read so, never rewritten elsewhere ([`Issues`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use ui::framework::Parts;

use tilo_rich_text::RichText;
use ui::tilo_reference::ResourceAt;
use ui::tilo_resource::items::Ref;
use ui::tilo_resource::{
    self, ActionCx, Bytes, Collection, Counter, Id, Model, NonBlank, Origin, Refusal, resource,
};

use crate::model::{
    Area, Attachment, BudgetBy, Issue, Link, MAX_ESTIMATE, MAX_MESSAGE_CHARS, MOST_PIECES_AT_ONCE,
    Message, NewIssue, Priority, Resumed, Status, Stopped, StoppedWhy, Tracker, WaitFor,
    checked_address, checked_assignee, checked_folder, checked_line, checked_waits,
};

/// The tracker's issues the engine keeps in one of its parts: every
/// issue made while it was the newest, and everything done to them since.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct IssueList {
    /// Issues
    ///
    /// Each issue is a storage unit of its own.
    #[model(unit)]
    pub issues: Collection<Issue>,
    /// The highest number given before this part started: its issues are
    /// numbered on from here.
    #[model(readonly)]
    pub before: u64,
    /// How many numbers this part gave. Increments merge, so a number given
    /// after a merge is past every issue either side made here.
    #[model(readonly)]
    pub numbers: Counter,
}

impl IssueList {
    /// The highest number this part gave, or the one before it started.
    #[must_use]
    pub fn last_number(&self) -> u64 {
        self.before
            .saturating_add(u64::try_from(self.numbers.get()).unwrap_or_default())
    }

    fn issue_mut(&mut self, issue: &Id) -> Result<&mut Issue, Refusal> {
        self.issues.get_mut(issue).ok_or_else(no_issue)
    }

    fn next_number(&mut self) -> u64 {
        self.numbers.add(1);
        self.last_number()
    }

    /// Make the issue `issue` says, as `create` does, and answer its id.
    fn make(&mut self, cx: &mut ActionCx, issue: NewIssue) -> Result<Id, Refusal> {
        let NewIssue {
            title,
            parent,
            body,
            status,
            assignee,
            url,
            project: _,
            priority,
            estimate,
            due_ms,
            labels,
            origin,
            rank,
            goal,
        } = issue;
        check_estimate(estimate)?;
        let origin = Origin::checked_option(origin)?;
        let assignee = checked_assignee(assignee)?;
        let author = Some(author(cx)?);
        let author_name = cx.acting_name();
        let number = self.next_number();
        let mut kept_labels = tilo_resource::Set::new();
        for label in labels.unwrap_or_default() {
            let label = label.trim();
            if !label.is_empty() {
                kept_labels.insert(label.to_owned());
            }
        }
        let given = given_to_agent(assignee.as_ref(), author.as_ref(), cx.now_ms());
        let issue = Issue {
            number,
            rank: rank.unwrap_or_default(),
            title: title.into_string(),
            body: RichText::from_markdown(body.unwrap_or_default()),
            status: status.unwrap_or_default(),
            assignee,
            url: url
                .map(|url| url.trim().to_owned())
                .filter(|url| !url.is_empty())
                .or_else(|| origin.as_ref().and_then(|origin| origin.url.clone())),
            parent: parent.map(|parent| Id::from_raw(parent.as_str().to_owned())),
            author,
            author_name,
            created_ms: cx.now_ms(),
            area: goal,
            priority: priority.and_then(Priority::level),
            estimate,
            due_ms,
            labels: kept_labels,
            origin,
            assigned_by: given.0,
            assigned_ms: given.1,
            ..Issue::default()
        };
        let id = cx.id();
        Ok(self.issues.push(id, issue))
    }

    /// What system search finds of the part: each issue, by its number,
    /// title, description and labels.
    #[must_use]
    pub fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        crate::model::search::entries(self.issues.iter())
    }

    /// What a part's issues hold, as [`Tracker::read_document`] answers.
    ///
    /// # Errors
    /// Refuses an item the part does not hold.
    pub fn read_document(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

/// What part `part` hands the part after it:
/// the number it starts counting from.
#[must_use]
pub fn carried(part: &IssueList) -> u64 {
    part.last_number()
}

fn no_issue() -> Refusal {
    Refusal::new("This issue no longer exists")
}

fn author(cx: &ActionCx) -> Result<tilo_resource::MemberId, Refusal> {
    cx.author()
        .cloned()
        .ok_or_else(|| Refusal::new("Sign in to do this"))
}

fn check_estimate(estimate: Option<u32>) -> Result<(), Refusal> {
    if estimate.is_some_and(|estimate| estimate > MAX_ESTIMATE) {
        return Err(Refusal::new("An estimate is at most 1000 points"));
    }
    Ok(())
}

/// Who gave an issue to `assignee` and when, when it is an agent and a person
/// gave it: the agent works on it for them. Nothing for anyone else.
fn given_to_agent(
    assignee: Option<&tilo_resource::Atom>,
    by: Option<&tilo_resource::MemberId>,
    at_ms: i64,
) -> (Option<tilo_resource::MemberId>, i64) {
    let to_agent = assignee.is_some_and(|assignee| assignee.0.starts_with("agent:"));
    match by {
        Some(by) if to_agent && by.is_person() => (Some(by.clone()), at_ms),
        _ => (None, 0),
    }
}

fn by_agent(cx: &ActionCx) -> bool {
    cx.invoked_by().is_some_and(|by| by.is_agent())
}

/// Who gives an issue when the action of `cx` assigns it: the person
/// themselves, or the one person the agent that assigns it acts for. An agent
/// acting for nobody, or for several people, gives it as itself, which asks
/// nobody's work of anyone.
fn giver(cx: &ActionCx) -> Result<tilo_resource::MemberId, Refusal> {
    let author = author(cx)?;
    if author.is_person() {
        return Ok(author);
    }
    let acted_for: Vec<tilo_resource::MemberId> = cx
        .invoked_by()
        .iter()
        .flat_map(|acting| acting.acted_for.iter())
        .filter_map(|who| who.by.member_id().cloned())
        .filter(tilo_resource::MemberId::is_person)
        .collect();
    match acted_for.as_slice() {
        [person] => Ok(person.clone()),
        _ => Ok(author),
    }
}

fn closed() -> Refusal {
    Refusal::new("This issue is closed")
}

/// Refuse `by` unless it is the agent `issue` is assigned to: its sessions are its
/// own to count.
fn its_agent(issue: &Issue, by: &tilo_resource::MemberId) -> Result<(), Refusal> {
    if by.is_agent() && issue.assigned_member().as_ref() == Some(by) {
        Ok(())
    } else {
        Err(Refusal::new(
            "Only the agent an issue is assigned to counts its sessions",
        ))
    }
}

/// `micros` millionths of the currency, as a refusal names an amount.
fn money(micros: u64) -> String {
    format!(
        "${}.{:02}",
        micros / 1_000_000,
        (micros % 1_000_000) / 10_000
    )
}

fn message_text(text: NonBlank) -> Result<String, Refusal> {
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(Refusal::new("This message is too long"));
    }
    Ok(text.into_string())
}

#[resource(
    kind = "tasks.issues",
    params = issue_list,
    search = IssueList::search,
    effects = crate::model::TasksEffect,
    read_content = IssueList::read_document
)]
impl IssueList {
    /// Create an issue.
    ///
    /// An issue moved from another tracker keeps where it came from in
    /// `origin`: its author and date there show as the issue's, and its
    /// history still says who moved it.
    #[action(writes = issues, step = "create")]
    pub fn create(
        &mut self,
        cx: &mut ActionCx,
        #[param(flatten)] issue: NewIssue,
    ) -> Result<(), Refusal> {
        self.make(cx, issue).map(drop)
    }

    /// Start counting this part's numbers on from `before`, the highest
    /// number the part before it gave. Written once, as the part starts.
    #[action(step = "create", housekeeping, writes = issues)]
    pub fn start(&mut self, before: u64) {
        self.before = self.before.max(before);
    }

    /// Create issues
    ///
    /// Several issues in one step, in the order given, each as `create`
    /// takes it. An issue may be a sub-issue of one made in the same step:
    /// its `under` names that one's `key`, or its place in the list.
    ///
    /// # Errors
    /// Refuses the whole list, naming the entry, when `create` would refuse
    /// one of its issues, or one stands under an issue that does not come
    /// before it in the list.
    #[action(step = "create", audience = "agents", writes = issues)]
    pub fn create_many(
        &mut self,
        cx: &mut ActionCx,
        #[param(
            "The issues, in order, each as `create` takes it. `key` names an issue within this call, and `under` is the key, or the place counted from 1, of the issue earlier in the list it is a sub-issue of"
        )]
        issues: Vec<crate::model::ListedIssue>,
    ) -> Result<(), Refusal> {
        many::check_count(issues.len())?;
        let under = many::nesting(&issues)?;
        let mut made: Vec<Id> = Vec::with_capacity(issues.len());
        for (at, (listed, under)) in issues.into_iter().zip(under).enumerate() {
            let title = listed.title.as_str().to_owned();
            many::check_listed(&listed).map_err(|refusal| many::entry(at, &title, refusal))?;
            let mut issue = listed.into_new();
            if let Some(parent) = under.and_then(|place| made.get(place)) {
                issue.parent = Some(Ref::from(parent));
            }
            let id = self
                .make(cx, issue)
                .map_err(|refusal| many::entry(at, &title, refusal))?;
            made.push(id);
        }
        Ok(())
    }

    /// Rename an issue: its title, as the board and its page show it.
    #[action(item = issues, field = "title", step = "edit", did = "renamed", writes = issues)]
    pub fn rename(&mut self, issue: &Id, title: NonBlank) -> Result<(), Refusal> {
        let title = title.into_string();
        if title.chars().count() > crate::model::MAX_TITLE_CHARS {
            return Err(Refusal::new("An issue's title fits on a line"));
        }
        title.trim().clone_into(&mut self.issue_mut(issue)?.title);
        Ok(())
    }

    /// Move an issue under another one, or to the top level.
    #[action(item = issues, step = "move", writes = issues)]
    pub fn move_under(&mut self, issue: &Id, parent: Option<Ref>) -> Result<(), Refusal> {
        if parent
            .as_ref()
            .is_some_and(|parent| parent.as_str() == issue.as_str())
        {
            return Err(Refusal::new(
                "An issue cannot sit under itself or its own sub-issues",
            ));
        }
        self.issue_mut(issue)?.parent =
            parent.map(|parent| Id::from_raw(parent.as_str().to_owned()));
        Ok(())
    }

    /// Assign an issue to a person or an agent, to someone not in the
    /// workspace yet by their name, or to nobody.
    ///
    /// Giving it to an agent asks the agent to do it, for whoever gave it:
    /// the agent works on it as their turn and answers in its thread. An
    /// agent that gives an issue to an agent gives it for the one person it
    /// acts for. An issue an agent filed for an agent (`create`) is a
    /// proposal nobody asked for yet: assigning it to that agent gives it.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn assign(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param(
            "a person's root key, agent:<id>, or outside:<name> for someone not in the workspace yet; nobody without"
        )]
        assignee: Option<String>,
    ) -> Result<(), Refusal> {
        let assignee = checked_assignee(assignee)?;
        let (by, at_ms) = given_to_agent(assignee.as_ref(), Some(&giver(cx)?), cx.now_ms());
        let target = self.issue_mut(issue)?;
        // Giving it to the agent it is with already asks nothing new, unless
        // nobody gave it yet.
        if target.assignee != assignee || by.is_none() || target.assigned_by.is_none() {
            target.assigned_by = by;
            target.assigned_ms = at_ms;
        }
        target.assignee = assignee;
        Ok(())
    }

    /// Move an issue into a project or an area, or out of every one.
    ///
    /// # Errors
    /// Refuses a project the tracker does not have.
    #[action(item = issues, step = "move", writes = issues)]
    pub fn set_project(
        &mut self,
        issue: &Id,
        #[param("The project or area it works towards, by its name or id; none without")]
        project: Option<Ref>,
        #[param("The project as the issue keeps it: filled in where it is invoked")] goal: Option<
            Area,
        >,
    ) -> Result<(), Refusal> {
        if project.is_some() && goal.is_none() {
            return Err(Tracker::no_area());
        }
        self.issue_mut(issue)?.area = goal;
        Ok(())
    }

    /// Attach a file to an issue: its bytes, or the content reference a put
    /// answered.
    ///
    /// # Errors
    /// Refuses an empty file or a name too long.
    #[action(item = issues, step = "create", writes = issues)]
    pub fn attach(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("Its file name")] name: NonBlank,
        #[param("Its bytes, inline or as a content reference (blake3:<hex>:<len>)")] bytes: Bytes,
        #[param("Its media type, such as image/png")] media_type: Option<String>,
    ) -> Result<(), Refusal> {
        if bytes == Bytes::default() {
            return Err(Refusal::new("An attachment has content"));
        }
        let name = name.into_string();
        if name.chars().count() > crate::model::MAX_ATTACHMENT_NAME_CHARS {
            return Err(Refusal::new("An attachment's name fits on a line"));
        }
        let author = Some(author(cx)?);
        let author_name = cx.acting_name();
        let at_ms = cx.now_ms();
        let id = cx.id();
        self.issue_mut(issue)?.attachments.push(
            id,
            Attachment {
                name,
                media_type: media_type
                    .map(|media_type| media_type.trim().to_owned())
                    .filter(|media_type| !media_type.is_empty()),
                bytes,
                author,
                author_name,
                at_ms,
            },
        );
        Ok(())
    }

    /// Take a file off an issue.
    #[action(item = issues, step = "delete", writes = issues)]
    pub fn detach(&mut self, issue: &Id, attachment: &Id) -> Result<(), Refusal> {
        self.issue_mut(issue)?
            .attachments
            .remove(attachment)
            .ok_or_else(|| Refusal::new("That file is not attached"))?;
        Ok(())
    }

    /// Label an issue.
    ///
    /// # Errors
    /// Refuses an issue at its 32 labels.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn add_label(&mut self, issue: &Id, label: NonBlank) -> Result<(), Refusal> {
        self.issue_mut(issue)?.labels.insert(label.into_string());
        Ok(())
    }

    /// Place an issue on the board: in a status column, before another
    /// issue or at the end.
    #[action(item = issues, step = "move", writes = issues)]
    pub fn place(
        &mut self,
        issue: &Id,
        status: Status,
        #[param("The issue it goes before, by its name or id; the end of the column without")]
        before: Option<Ref>,
        #[param("Its place in the tracker's order: filled in where it is invoked")] rank: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let _ = before;
        let target = self.issue_mut(issue)?;
        target.status = status;
        if let Some(rank) = rank {
            target.rank = rank;
        }
        Ok(())
    }

    /// Comment on an issue. A comment moved from another tracker keeps
    /// where it came from in `origin`.
    #[action(item = issues, field = "comments", step = "write", writes = issues)]
    pub fn comment(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("What it says, in markdown")] text: NonBlank,
        #[param(
            "Where it was first written, when it is moved here from elsewhere: shown as its author and date, while its history keeps who moved it"
        )]
        origin: Option<Origin>,
    ) -> Result<(), Refusal> {
        let text = message_text(text)?;
        let origin = Origin::checked_option(origin)?;
        let author = Some(author(cx)?);
        let author_name = cx.acting_name();
        let agent = by_agent(cx);
        let at_ms = cx.now_ms();
        let id = cx.id();
        self.issue_mut(issue)?.comments.push(
            id,
            Message {
                author,
                author_name,
                by_agent: agent,
                at_ms,
                text,
                origin,
                ..Message::default()
            },
        );
        Ok(())
    }

    /// Link an issue to another resource.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn link(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The app that keeps the resource")] app: NonBlank,
        #[param("The resource's kind")] kind: String,
        #[param("The resource's address in its app")] resource: NonBlank,
        #[param("How the link reads")] label: String,
        #[param("The app's resource that keeps it; its resource of the workspace without")]
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
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn link_note(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("The note", resource = "notes.pages.pages")] note: ResourceAt,
    ) -> Result<(), Refusal> {
        let (Some(app), Some(resource)) = (
            NonBlank::parse(&note.app),
            NonBlank::parse(&note.reference.id),
        ) else {
            return Err(Refusal::new("That is not a note"));
        };
        if note.app != crate::model::NOTES || note.reference.kind != crate::model::NOTE_KIND {
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

    /// Delete an issue. Its sub-issues stand at the top level.
    #[action(item = issues, step = "delete", destructive, writes = issues)]
    pub fn delete(&mut self, issue: &Id) -> Result<(), Refusal> {
        self.issues.remove(issue).ok_or_else(no_issue)?;
        Ok(())
    }

    /// Plan the issue: the goal it works towards, how urgent it is, how much
    /// work it is and when it is wanted. Each argument is optional, and the
    /// one given is the one written; the others stay as they were.
    ///
    /// # Errors
    /// Refuses a goal that is not a resource, or an estimate over 1000.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn plan(
        &mut self,
        issue: &Id,
        #[param("The resource it works towards: resource, app, kind and label")] area: Option<Area>,
        #[param("How urgent it is: none, urgent, high, medium or low")] priority: Option<Priority>,
        #[param("How much work it is, in points")] estimate: Option<u32>,
        #[param("When it is wanted, in milliseconds since the epoch")] due_ms: Option<i64>,
    ) -> Result<(), Refusal> {
        check_estimate(estimate)?;
        let target = self.issue_mut(issue)?;
        if let Some(area) = area {
            if !area.names_a_resource() {
                return Err(Refusal::new("A goal is a resource of the workspace"));
            }
            target.area = Some(area);
        }
        if let Some(priority) = priority {
            target.priority = priority.level();
        }
        if let Some(estimate) = estimate {
            target.estimate = Some(estimate);
        }
        if let Some(due_ms) = due_ms {
            target.due_ms = Some(due_ms);
        }
        Ok(())
    }

    /// Set place
    ///
    /// Say where an issue's working material is kept: a folder in the
    /// workspace's files, and the note its plan, its findings and what is
    /// still open are written in. A later session of whoever works on it starts
    /// from them.
    ///
    /// # Errors
    /// Refuses a path that is no folder's.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn set_place(
        &mut self,
        issue: &Id,
        #[param("The folder in the workspace's files, as a path: Tasks/12 Market analysis")]
        folder: String,
        #[param("The note in Notes, by its resource; as it was without")] note: Option<String>,
    ) -> Result<(), Refusal> {
        let folder = checked_folder(&folder)?;
        let note = checked_address(note)?;
        let work = self.issue_mut(issue)?.work.get_or_insert_default();
        work.folder = folder;
        if let Some(note) = note {
            work.note = note;
        }
        Ok(())
    }

    /// Wait
    ///
    /// Let an issue wait: for a reply, a resource to change, a person's answer,
    /// several people, or only a moment. Any one of them ends the wait, and
    /// the moment always does. Nothing works on it while it waits.
    ///
    /// # Errors
    /// Refuses a closed issue, a wait that says nothing of what it is for,
    /// and a moment that is not later than now.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn wait(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param(
            "What it waits for, each with a kind (time, reply, change, answer or people), what in a few words, and optionally what is watched (on) and the event that can end it early; none to wait for the moment alone"
        )]
        waits: Vec<WaitFor>,
        #[param("The latest moment the wait ends, in milliseconds since the epoch")] until_ms: i64,
    ) -> Result<(), Refusal> {
        let waits = checked_waits(waits)?;
        let now = cx.now_ms();
        if until_ms <= now {
            return Err(Refusal::new("A wait ends later than now"));
        }
        let target = self.issue_mut(issue)?;
        if target.status.closed() {
            return Err(closed());
        }
        let work = target.work.get_or_insert_default();
        work.waits = waits;
        work.until_ms = Some(until_ms);
        work.since_ms = Some(now);
        work.stopped = None;
        Ok(())
    }

    /// Resume
    ///
    /// Tell an issue's work to go on: what it waited for came, or its person
    /// says so after it stopped. Whoever works on it is asked again, and
    /// starts from its place.
    ///
    /// # Errors
    /// Refuses a closed issue, and one whose budget is used up: that one
    /// goes on once its budget is raised.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn resume(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("What lets it go on, in a few words: the supplier replied")] cause: NonBlank,
    ) -> Result<(), Refusal> {
        let cause = checked_line(cause.as_str(), "what lets it go on")?;
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        if target.status.closed() {
            return Err(closed());
        }
        let work = target.work.get_or_insert_default();
        match work.used_up() {
            Some(StoppedWhy::Time) => {
                return Err(Refusal::new(
                    "Its working time is used up: give it more time first",
                ));
            }
            Some(_) => {
                return Err(Refusal::new(format!(
                    "Its budget of {} is spent: raise it first",
                    money(work.budget_micros.unwrap_or_default())
                )));
            }
            None => {}
        }
        work.end_wait();
        work.stopped = None;
        work.lost = 0;
        work.resumed = Some(Resumed { cause, at_ms: now });
        Ok(())
    }

    /// Set budget
    ///
    /// Say what an issue may cost over all its sessions, in money, in working
    /// time, or both; what is left out stays as it was. A person sets and
    /// raises it. The agent working on it may propose one where none is set
    /// and may lower one, and holds itself to it.
    ///
    /// # Errors
    /// Refuses an agent raising a budget a person set.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn set_budget(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("What the whole issue may cost, in millionths of the currency")]
        money_micros: Option<u64>,
        #[param("How long it may be worked on over all its sessions, in milliseconds")]
        time_ms: Option<u64>,
    ) -> Result<(), Refusal> {
        if money_micros.is_none() && time_ms.is_none() {
            return Err(Refusal::new("Give the money or the time it may take"));
        }
        let agent = by_agent(cx);
        let work = self.issue_mut(issue)?.work.get_or_insert_default();
        let kept = work.budget_micros.is_some() || work.budget_ms.is_some();
        let raises = |new: Option<u64>, old: Option<u64>| matches!((new, old), (Some(new), Some(old)) if new > old);
        if agent
            && kept
            && work.budget_by == BudgetBy::Person
            && (raises(money_micros, work.budget_micros) || raises(time_ms, work.budget_ms))
        {
            return Err(Refusal::new("Only a person raises a budget a person set"));
        }
        if let Some(money) = money_micros {
            work.budget_micros = Some(money);
        }
        if let Some(time) = time_ms {
            work.budget_ms = Some(time);
        }
        if !agent {
            work.budget_by = BudgetBy::Person;
        } else if !kept {
            work.budget_by = BudgetBy::Agent;
        }
        Ok(())
    }

    /// Work pieces at once
    ///
    /// Say how many of an issue's pieces, the issues under it, are worked on
    /// at once by the agents they are given to: the others are begun as those
    /// in work are through. Left out, any number is. A person says it, or the
    /// agent the issue is given to.
    ///
    /// # Errors
    /// Refuses none at a time, more than the most there may be, and an agent
    /// the issue is not given to.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn set_pieces_at_once(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("How many of its pieces are in work at once; left out for any number")]
        at_once: Option<u64>,
    ) -> Result<(), Refusal> {
        if at_once.is_some_and(|at_once| at_once == 0 || at_once > MOST_PIECES_AT_ONCE) {
            return Err(Refusal::new(format!(
                "Between 1 and {MOST_PIECES_AT_ONCE} pieces are in work at once"
            )));
        }
        let by = author(cx)?;
        let target = self.issue_mut(issue)?;
        if by.is_agent() && target.assigned_member().as_ref() != Some(&by) {
            return Err(Refusal::new(
                "Only a person or its own agent says how many of an issue's pieces are in work",
            ));
        }
        target.work.get_or_insert_default().pieces_at_once = at_once;
        Ok(())
    }

    /// Session began
    ///
    /// The agent an issue is assigned to says a session of it on the issue began:
    /// the issue no longer waits and is no longer stopped. A session that began
    /// before and never ended is counted as lost.
    ///
    /// # Errors
    /// Refuses anyone but the agent the issue is assigned to.
    #[action(item = issues, step = "edit", audience = "agents", writes = issues)]
    pub fn session_began(&mut self, cx: &mut ActionCx, issue: &Id) -> Result<(), Refusal> {
        let now = cx.now_ms();
        let by = author(cx)?;
        let target = self.issue_mut(issue)?;
        its_agent(target, &by)?;
        let work = target.work.get_or_insert_default();
        if work.session_since_ms.is_some() {
            work.lost = work.lost.saturating_add(1);
        }
        work.sessions = work.sessions.saturating_add(1);
        work.session_since_ms = Some(now);
        work.end_wait();
        work.stopped = None;
        Ok(())
    }

    /// Session ended
    ///
    /// The agent an issue is assigned to says its session ended, what it cost and
    /// how long it worked: both count against the issue's budget.
    ///
    /// # Errors
    /// Refuses anyone but the agent the issue is assigned to.
    #[action(item = issues, step = "edit", audience = "agents", writes = issues)]
    pub fn session_ended(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("What the session cost, in millionths of the currency")] cost_micros: u64,
        #[param("How long it worked, in milliseconds")] worked_ms: u64,
    ) -> Result<(), Refusal> {
        let by = author(cx)?;
        let target = self.issue_mut(issue)?;
        its_agent(target, &by)?;
        let work = target.work.get_or_insert_default();
        work.spent_micros = work.spent_micros.saturating_add(cost_micros);
        work.worked_ms = work.worked_ms.saturating_add(worked_ms);
        work.session_since_ms = None;
        work.lost = 0;
        Ok(())
    }

    /// Stop
    ///
    /// Say that an issue's work stopped before it was finished, and why, in a
    /// plain line its person reads: its budget is spent, its worker was lost,
    /// it failed, or a person stopped it. It reads as stopped, never as done,
    /// until it is told to go on.
    ///
    /// # Errors
    /// Refuses a closed issue.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn stop(
        &mut self,
        cx: &mut ActionCx,
        issue: &Id,
        #[param("Why: failed, budget, time, lost-worker or person")] why: StoppedWhy,
        #[param("The plain line its person reads")] words: NonBlank,
    ) -> Result<(), Refusal> {
        let words = checked_line(words.as_str(), "why it stopped")?;
        let now = cx.now_ms();
        let target = self.issue_mut(issue)?;
        if target.status.closed() {
            return Err(closed());
        }
        let work = target.work.get_or_insert_default();
        work.end_wait();
        work.session_since_ms = None;
        work.stopped = Some(Stopped {
            why,
            words,
            at_ms: now,
        });
        Ok(())
    }
}

/// Every issue the held parts keep, read as one tracker: in the tracker's
/// order, each found by its id, with what reads across parts (a parent that
/// is gone, a project that was deleted) resolved the same way everywhere.
///
/// Kept, not rebuilt: [`Self::sync`] reads only the parts that changed since
/// it last read them, issue by issue, and moves those issues in the order,
/// among their parent's sub-issues and in the tallies the sidebar counts. A
/// change to one issue costs the part holding it, however many issues there
/// are.
#[derive(Default)]
pub struct Issues {
    /// The parts as last read, oldest first.
    parts: Vec<(u64, Rc<IssueList>)>,
    /// The slice they were read from, so reading the same parts again is free.
    synced: Option<Parts<IssueList>>,
    /// Each issue held: the part keeping it and what it was filed under.
    held: HashMap<Id, Filed>,
    /// Every issue in the tracker's order.
    order: BTreeSet<Place>,
    /// The issues naming each parent, whether or not that parent is held.
    named: HashMap<Id, BTreeSet<Place>>,
    /// The issues at the top: those naming no parent, or one not held.
    top: BTreeSet<Place>,
    by_number: HashMap<u64, Id>,
    /// How many issues there are of each kind a filter tells apart: started
    /// or not, and whom they are assigned to.
    kinds: HashMap<(bool, Option<String>), usize>,
    /// How many issues carry each label.
    labels: BTreeMap<String, usize>,
    revision: u64,
    /// What the read under way touched so far.
    touching: Vec<u64>,
    /// The numbers of the issues each recent revision moved, changed or took
    /// out, and of their parents, whose progress moved with them: what a list
    /// draws again, without drawing every row.
    touched: std::collections::VecDeque<(u64, Vec<u64>)>,
}

/// How many revisions back [`Issues::touched_since`] can tell what moved.
const TOUCHED_KEPT: usize = 64;

/// Where an issue stands in the tracker's order: by rank, then number, then id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Place {
    rank: String,
    number: u64,
    id: Id,
}

/// What an issue was filed under, to take it out again without reading it.
struct Filed {
    part: u64,
    place: Place,
    parent: Option<Id>,
    kind: (bool, Option<String>),
    labels: Vec<String>,
}

impl Issues {
    /// The issues `parts` keep, oldest part first. An issue two parts both
    /// claim (never written so, but read safely) is the newer part's.
    #[must_use]
    pub fn of(parts: &[(u64, Rc<IssueList>)]) -> Self {
        let mut issues = Self::default();
        issues.read(parts);
        issues
    }

    /// Follow `parts`: what changed since the last parts read, and only that.
    /// Whether anything did.
    pub fn sync(&mut self, parts: &Parts<IssueList>) -> bool {
        if self
            .synced
            .as_ref()
            .is_some_and(|synced| Rc::ptr_eq(synced, parts))
        {
            return false;
        }
        let _profile = ui::tilo_resource::profiling::Phase::start("tasks.issues_sync");
        self.synced = Some(Rc::clone(parts));
        self.read(parts)
    }

    /// Whether `parts` are the parts last read.
    #[must_use]
    pub fn follows(&self, parts: &Parts<IssueList>) -> bool {
        self.synced
            .as_ref()
            .is_some_and(|synced| Rc::ptr_eq(synced, parts))
    }

    /// A number that moves whenever an issue does.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn read(&mut self, parts: &[(u64, Rc<IssueList>)]) -> bool {
        let before = std::mem::replace(&mut self.parts, parts.to_vec());
        let earlier: HashMap<u64, Rc<IssueList>> = before.into_iter().collect();
        let mut changed = false;
        self.touching = Vec::new();
        let now: std::collections::HashSet<u64> = parts.iter().map(|(index, _)| *index).collect();
        // Parts no longer held take their issues with them.
        for (index, part) in &earlier {
            if !now.contains(index) {
                for (id, _) in part.issues.iter() {
                    changed |= self.remove(id, *index);
                }
            }
        }
        for (index, part) in parts {
            match earlier.get(index) {
                Some(last) if Rc::ptr_eq(last, part) => {}
                Some(last) => {
                    for (id, _) in last.issues.iter() {
                        if part.issues.get(id).is_none() {
                            changed |= self.remove(id, *index);
                        }
                    }
                    for (id, issue) in part.issues.iter() {
                        if last.issues.get(id) != Some(issue) {
                            self.remove(id, *index);
                            changed |= self.insert(id, issue, *index);
                        }
                    }
                }
                None => {
                    for (id, issue) in part.issues.iter() {
                        changed |= self.insert(id, issue, *index);
                    }
                }
            }
        }
        if changed {
            self.revision = self.revision.wrapping_add(1);
            let touched = std::mem::take(&mut self.touching);
            self.touched.push_back((self.revision, touched));
            while self.touched.len() > TOUCHED_KEPT {
                self.touched.pop_front();
            }
        }
        changed
    }

    /// The numbers of the issues that moved, changed or went since `revision`, and of their
    /// parents; `None` when that is too far back to tell, and everything may have.
    #[must_use]
    pub fn touched_since(&self, revision: u64) -> Option<Vec<u64>> {
        if revision == self.revision {
            return Some(Vec::new());
        }
        let first = self.touched.front()?.0;
        if revision.wrapping_add(1) < first {
            return None;
        }
        Some(
            self.touched
                .iter()
                .filter(|(at, _)| *at > revision)
                .flat_map(|(_, numbers)| numbers.iter().copied())
                .collect(),
        )
    }

    /// Note that the issue numbered `number` moved, and its parent `parent` with it.
    fn touch(&mut self, number: u64, parent: Option<&Id>) {
        self.touching.push(number);
        if let Some(filed) = parent.and_then(|parent| self.held.get(parent)) {
            self.touching.push(filed.place.number);
        }
    }

    /// The issue after `id` in the tracker's order.
    #[must_use]
    pub fn after(&self, id: &Id) -> Option<&Id> {
        let place = &self.held.get(id)?.place;
        self.order
            .range((std::ops::Bound::Excluded(place), std::ops::Bound::Unbounded))
            .next()
            .map(|next| &next.id)
    }

    /// File `issue`, kept in part `part`, unless a newer part keeps it.
    fn insert(&mut self, id: &Id, issue: &Issue, part: u64) -> bool {
        if let Some(filed) = self.held.get(id) {
            if filed.part > part {
                return false;
            }
            let older = filed.part;
            self.remove(id, older);
        }
        let place = Place {
            rank: issue.rank.clone(),
            number: issue.number,
            id: id.clone(),
        };
        let kind = (
            issue.status.started(),
            issue.assignee.as_ref().map(|assignee| assignee.0.clone()),
        );
        let count = self.kinds.entry(kind.clone()).or_default();
        *count = count.saturating_add(1);
        let labels: Vec<String> = issue.labels.iter().cloned().collect();
        for label in &labels {
            let count = self.labels.entry(label.clone()).or_default();
            *count = count.saturating_add(1);
        }
        self.order.insert(place.clone());
        match &issue.parent {
            Some(parent) => {
                self.named
                    .entry(parent.clone())
                    .or_default()
                    .insert(place.clone());
                if !self.held.contains_key(parent) {
                    self.top.insert(place.clone());
                }
            }
            None => {
                self.top.insert(place.clone());
            }
        }
        // Its own sub-issues stood at the top while it was not held: they move
        // under it.
        if let Some(under) = self.named.get(id) {
            for child in under {
                self.top.remove(child);
                self.touching.push(child.number);
            }
        }
        self.by_number.insert(issue.number, id.clone());
        self.touch(issue.number, issue.parent.as_ref());
        self.held.insert(
            id.clone(),
            Filed {
                part,
                place,
                parent: issue.parent.clone(),
                kind,
                labels,
            },
        );
        true
    }

    /// Take out the issue `id` as part `part` filed it. Where another part
    /// still keeps it, that part's is filed instead.
    fn remove(&mut self, id: &Id, part: u64) -> bool {
        if self.held.get(id).is_none_or(|filed| filed.part != part) {
            return false;
        }
        let Some(Filed {
            place,
            parent,
            kind,
            labels,
            ..
        }) = self.held.remove(id)
        else {
            return false;
        };
        if let Some(count) = self.kinds.get_mut(&kind) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.kinds.remove(&kind);
            }
        }
        for label in &labels {
            if let Some(count) = self.labels.get_mut(label) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.labels.remove(label);
                }
            }
        }
        self.order.remove(&place);
        self.top.remove(&place);
        if let Some(parent) = &parent
            && let Some(under) = self.named.get_mut(parent)
        {
            under.remove(&place);
            if under.is_empty() {
                self.named.remove(parent);
            }
        }
        if self.by_number.get(&place.number) == Some(id) {
            self.by_number.remove(&place.number);
        }
        self.touch(place.number, parent.as_ref());
        // Its sub-issues stand at the top while it is gone: they moved too.
        if let Some(under) = self.named.get(id) {
            self.top.extend(under.iter().cloned());
            self.touching.extend(under.iter().map(|place| place.number));
        }
        // An older part keeping it too is read again.
        let older = self
            .parts
            .iter()
            .rev()
            .filter(|(index, _)| *index != part)
            .find_map(|(index, kept)| kept.issues.get(id).map(|issue| (*index, issue.clone())));
        if let Some((index, issue)) = older {
            self.insert(id, &issue, index);
        }
        true
    }

    fn part(&self, index: u64) -> Option<&IssueList> {
        self.parts
            .iter()
            .find(|(held, _)| *held == index)
            .map(|(_, part)| &**part)
    }

    /// How many issues there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Every issue, in the tracker's order.
    pub fn iter(&self) -> impl Iterator<Item = (&Id, &Issue)> + '_ {
        self.order
            .iter()
            .filter_map(|place| Some((&place.id, self.get(&place.id)?)))
    }

    /// The issue `id`, if one is.
    #[must_use]
    pub fn get(&self, id: &Id) -> Option<&Issue> {
        let filed = self.held.get(id)?;
        self.part(filed.part)?.issues.get(id)
    }

    /// Whether `id` is an issue here.
    #[must_use]
    pub fn contains(&self, id: &Id) -> bool {
        self.held.contains_key(id)
    }

    /// The part that keeps the issue `id`.
    #[must_use]
    pub fn part_of(&self, id: &Id) -> Option<u64> {
        self.held.get(id).map(|filed| filed.part)
    }

    /// The issue numbered `number`, if one is.
    #[must_use]
    pub fn by_number(&self, number: u64) -> Option<(&Id, &Issue)> {
        let id = self.by_number.get(&number)?;
        Some((id, self.get(id)?))
    }

    /// The top-level issue the issue numbered `number` stands under, itself
    /// when it stands at the top: read up its parents, not across the tracker.
    #[must_use]
    pub fn top_of(&self, number: u64) -> Option<u64> {
        let mut id = self.by_number.get(&number)?;
        // An issue stands at the top, or under the held parent it names; the
        // bound keeps a cycle the parts were written into from looping.
        for _ in 0..=self.held.len() {
            let filed = self.held.get(id)?;
            if self.top.contains(&filed.place) {
                return Some(filed.place.number);
            }
            id = self
                .held
                .get_key_value(filed.parent.as_ref()?)
                .map(|(parent, _)| parent)?;
        }
        None
    }

    /// The top-level issue after the top-level issue numbered `number`, in
    /// the tracker's order; `None` after the last, or when `number` is not
    /// at the top.
    #[must_use]
    pub fn top_after(&self, number: u64) -> Option<u64> {
        let place = &self.held.get(self.by_number.get(&number)?)?.place;
        if !self.top.contains(place) {
            return None;
        }
        self.top
            .range((std::ops::Bound::Excluded(place), std::ops::Bound::Unbounded))
            .next()
            .map(|place| place.number)
    }

    /// The parent `issue` stands under: the one it names, while that one is
    /// here.
    #[must_use]
    pub fn parent_of(&self, issue: &Issue) -> Option<&Id> {
        issue
            .parent
            .as_ref()
            .and_then(|parent| self.held.get_key_value(parent).map(|(id, _)| id))
    }

    /// The direct sub-issues of `parent` (top-level issues for `None`), in
    /// the tracker's order.
    pub fn children<'s>(
        &'s self,
        parent: Option<&Id>,
    ) -> impl Iterator<Item = (&'s Id, &'s Issue)> + 's {
        let places = match parent {
            None => Some(&self.top),
            Some(parent) if self.held.contains_key(parent) => self.named.get(parent),
            Some(_) => None,
        };
        places
            .into_iter()
            .flatten()
            .filter_map(|place| Some((&place.id, self.get(&place.id)?)))
    }

    /// How many of `parent`'s direct sub-issues are closed, of how many.
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
        let mut at = Some(issue);
        // A parent chain longer than the tracker is a cycle a merge made:
        // stop rather than loop.
        for _ in 0..=self.held.len() {
            match at {
                Some(id) if id == ancestor => return true,
                Some(id) => at = self.get(id).and_then(|issue| self.parent_of(issue)),
                None => return false,
            }
        }
        false
    }

    /// How many issues `matches` takes, read from the tallies kept as issues
    /// come and go: one question per kind of issue a filter tells apart (started or
    /// not, and whom it is assigned to), not one per issue.
    pub fn count_kinds(&self, matches: impl Fn(&Issue) -> bool) -> usize {
        self.kinds
            .iter()
            .filter(|((started, assignee), _)| {
                let issue = Issue {
                    status: if *started {
                        Status::InProgress
                    } else {
                        Status::Todo
                    },
                    assignee: assignee.clone().map(ui::tilo_resource::Atom),
                    ..Issue::default()
                };
                matches(&issue)
            })
            .map(|(_, count)| *count)
            .sum()
    }

    /// Every label the issues carry, in the order a person reads them, with
    /// how many carry it.
    pub fn labels(&self) -> impl Iterator<Item = (&str, usize)> + '_ {
        self.labels
            .iter()
            .map(|(label, count)| (label.as_str(), *count))
    }

    /// The goal `issue` works towards: what it names, unless that is one of
    /// the tracker's projects that `tracker` no longer has.
    #[must_use]
    pub fn goal_of<'t>(tracker: &Tracker, issue: &'t Issue) -> Option<&'t Area> {
        issue.area.as_ref().filter(|area| {
            area.kind != crate::model::KIND
                || tracker.area(&Id::from_raw(area.resource.clone())).is_some()
        })
    }

    /// The issues that work towards `area`, in the tracker's order: the tasks
    /// the project holds.
    pub fn in_area<'s>(&'s self, area: &'s Id) -> impl Iterator<Item = (&'s Id, &'s Issue)> + 's {
        self.iter().filter(move |(_, issue)| {
            issue.area.as_ref().is_some_and(|named| {
                named.kind == crate::model::KIND && named.resource == area.as_str()
            })
        })
    }
}

/// The issues the held parts keep, looked up part by part without reading
/// them into one list: what an action checks where it is invoked, at a cost
/// of the parts held, not of every issue sorted.
struct Held<'a>(&'a [(u64, Rc<IssueList>)]);

impl<'a> Held<'a> {
    fn get(&self, id: &Id) -> Option<&'a Issue> {
        self.0
            .iter()
            .rev()
            .find_map(|(_, part)| part.issues.get(id))
    }

    fn parent_of(&self, issue: &'a Issue) -> Option<&'a Id> {
        issue
            .parent
            .as_ref()
            .filter(|parent| self.get(parent).is_some())
    }

    fn is_under(&self, issue: &Id, ancestor: &Id) -> bool {
        let mut at = Some(issue);
        let bound: usize = self.0.iter().map(|(_, part)| part.issues.len()).sum();
        for _ in 0..=bound {
            match at {
                Some(id) if id == ancestor => return true,
                Some(id) => at = self.get(id).and_then(|issue| self.parent_of(issue)),
                None => return false,
            }
        }
        false
    }

    fn ranks(&self) -> impl Iterator<Item = (&'a Id, &'a Issue)> + '_ {
        self.0.iter().flat_map(|(_, part)| part.issues.iter())
    }

    /// The rank after every issue's.
    fn last_rank(&self) -> String {
        let last = self
            .ranks()
            .map(|(_, issue)| issue.rank.as_str())
            .filter(|rank| !rank.is_empty())
            .max();
        tilo_resource::order::between(last, None)
    }

    /// The rank of an issue placed just before `before`, `moving` left out
    /// of where it stands now: between `before` and the issue ordered right
    /// before it.
    fn rank_before(&self, before: &Id, moving: &Id) -> String {
        let Some(high) = self.get(before) else {
            return self.last_rank();
        };
        let key = |id: &Id, issue: &Issue| (issue.rank.clone(), issue.number, id.clone());
        let bound = key(before, high);
        let low = self
            .ranks()
            .filter(|(id, _)| *id != moving)
            .filter(|(id, issue)| key(id, issue) < bound)
            .max_by(|(a_id, a), (b_id, b)| key(a_id, a).cmp(&key(b_id, b)))
            .map(|(_, issue)| issue.rank.as_str())
            .filter(|rank| !rank.is_empty());
        let high = Some(high.rank.as_str()).filter(|rank| !rank.is_empty());
        match (low, high) {
            // Equal ranks (issues made at once) leave no room between them:
            // it goes after the lower one.
            (Some(low), Some(high)) if low >= high => {
                tilo_resource::order::between(Some(low), None)
            }
            (low, high) => tilo_resource::order::between(low, high),
        }
    }
}

/// The project or area `project` names, as an issue keeps it.
///
/// # Errors
/// Refuses a name the tracker has no project or area of.
fn goal_named(tracker: &Tracker, project: &Ref) -> Result<Area, String> {
    let area = tracker
        .area_named(project.as_str())
        .map_err(|refusal| refusal.to_string())?;
    let named = tracker
        .area(&area)
        .ok_or_else(|| Tracker::no_area().to_string())?;
    Ok(crate::model::area_goal(&area, &named.name))
}

/// Fill in, where `actions` are invoked together, what each reads of the
/// tracker's projects and of every issue held there, and refuse what they do
/// not allow: a parent that is not an issue, a project the tracker does not
/// have, an issue placed under its own sub-issue. The issues are read once for
/// all of them.
///
/// # Errors
/// Refuses as the action's verb would, had it every part.
pub fn prepare(
    actions: &mut [<IssueList as tilo_resource::Resource>::Action],
    tracker: &Tracker,
    parts: &[(u64, Rc<IssueList>)],
) -> Result<(), String> {
    let held = Held(parts);
    let mut last = Last::after(&held);
    for action in actions {
        prepare_one(action, tracker, &held, &mut last)?;
    }
    Ok(())
}

/// The ranks after every issue held, one after the other: where the issues
/// that actions invoked together make or move to the end go, each after the
/// one before it. Every issue is scanned once, for the first.
pub(super) struct Last<'h, 'a> {
    held: &'h Held<'a>,
    given: Option<String>,
}

impl<'h, 'a> Last<'h, 'a> {
    const fn after(held: &'h Held<'a>) -> Self {
        Self { held, given: None }
    }

    /// The rank after every issue's and every rank given before.
    pub(super) fn next(&mut self) -> String {
        let next = match &self.given {
            Some(given) => tilo_resource::order::between(Some(given), None),
            None => self.held.last_rank(),
        };
        self.given = Some(next.clone());
        next
    }
}

fn prepare_one(
    action: &mut <IssueList as tilo_resource::Resource>::Action,
    tracker: &Tracker,
    held: &Held<'_>,
    last: &mut Last<'_, '_>,
) -> Result<(), String> {
    use issue_list::{Create, MoveUnder, Place, SetProject};
    // The issue an action names in its own parameter may be written as its
    // title, its number or an address too: it is resolved here, where every
    // part is held, so the part keeping it can be found.
    if let Some(issue) = item_of_mut(&mut action.operation)
        && held.get(issue).is_none()
        && ui::tilo_actions::params::made_of_marker(issue.as_str()).is_none()
    {
        *issue = held.named(issue.as_str())?;
    }
    // A reference to an item an earlier member of the same group makes
    // (`{"$made": …}`) is left as it is: the item does not exist here yet, and
    // the group fills the reference with its id as that member's part applies.
    let check_parent = |parent: Option<&Ref>| -> Result<Option<Id>, String> {
        parent
            .filter(|parent| ui::tilo_actions::params::made_of_marker(parent.as_str()).is_none())
            .map(|parent| held.named(parent.as_str()))
            .transpose()
    };
    let goal = |project: Option<&Ref>| -> Result<Option<Area>, String> {
        project
            .map(|project| goal_named(tracker, project))
            .transpose()
    };
    match &mut action.operation {
        IssueListAction::Create(Create { issue }) => {
            if let Some(parent) = issue.parent.as_mut()
                && let Some(id) = check_parent(Some(parent))?
            {
                *parent = Ref::from(id);
            }
            issue.goal = goal(issue.project.as_ref())?;
            issue.rank = Some(last.next());
        }
        IssueListAction::MoveUnder(MoveUnder { issue, parent }) => {
            let resolved = check_parent(parent.as_ref())?;
            if let Some(parent) = &resolved
                && held.is_under(parent, issue)
            {
                return Err("An issue cannot sit under itself or its own sub-issues".into());
            }
            *parent = resolved.map(Ref::from);
        }
        IssueListAction::SetProject(SetProject {
            project, goal: to, ..
        }) => {
            *to = goal(project.as_ref())?;
        }
        IssueListAction::Place(Place {
            issue,
            before,
            rank,
            ..
        }) => {
            let named_before = check_parent(before.as_ref())?;
            *rank = Some(
                match named_before.as_ref().filter(|before| *before != issue) {
                    Some(before) => held.rank_before(before, issue),
                    None => last.next(),
                },
            );
            *before = named_before.map(Ref::from);
        }
        IssueListAction::CreateMany(listed) => {
            many::prepare_create(&mut listed.issues, tracker, held, last)?;
        }
        // What `assign` and `comment` refuse of their arguments is refused
        // here too, where every part is held, naming the issue: several of
        // them invoked as one group are then written to every part they
        // name or to none.
        IssueListAction::Assign(issue_list::Assign { issue, assignee }) => {
            checked_assignee(assignee.clone())
                .map_err(|refusal| format!("{}: {refusal}", held.reads_as(issue)))?;
        }
        IssueListAction::Comment(issue_list::Comment {
            issue,
            text,
            origin,
        }) => {
            if text.as_str().chars().count() > MAX_MESSAGE_CHARS {
                return Err(format!(
                    "{}: This message is too long",
                    held.reads_as(issue)
                ));
            }
            Origin::checked_option(origin.clone())
                .map_err(|refusal| format!("{}: {refusal}", held.reads_as(issue)))?;
        }
        _ => {}
    }
    Ok(())
}

impl Held<'_> {
    /// The issue `reference` names among the parts: its id, the number it
    /// reads as (`#12`), an address it is published under, or its title.
    /// A reference naming none is refused with the issues there are, so a
    /// mistyped or renamed issue is never reported as one that went away.
    fn named(&self, reference: &str) -> Result<Id, String> {
        let asked = reference.trim();
        if asked.is_empty() {
            return Err("name the issue, by its id or its name".to_owned());
        }
        if let Some(number) = crate::model::search::number(asked)
            && let Some((id, _)) = self.issue_number(number)
        {
            return Ok(id.clone());
        }
        let by_id = |candidate: &str| {
            let id = Id::from_raw(candidate);
            self.get(&id).map(|_| id)
        };
        if let Some(id) = by_id(asked) {
            return Ok(id);
        }
        for candidate in reference_parts(asked) {
            if let Some(id) = by_id(&candidate) {
                return Ok(id);
            }
        }
        let named: Vec<&Id> = self
            .ranks()
            .filter(|(_, issue)| issue.title.eq_ignore_ascii_case(asked))
            .map(|(id, _)| id)
            .collect();
        match named.as_slice() {
            [id] => return Ok((*id).clone()),
            [_, ..] => {
                let ids: Vec<&str> = named.iter().map(|id| id.as_str()).collect();
                return Err(format!(
                    "more than one issue is named {asked}; name it by its id: {}",
                    ids.join(", ")
                ));
            }
            [] => {}
        }
        let titles: Vec<&str> = self
            .ranks()
            .map(|(_, issue)| issue.title.as_str())
            .take(12)
            .collect();
        let more = self.ranks().count().saturating_sub(titles.len());
        let holds = if titles.is_empty() {
            "none yet".to_owned()
        } else if more > 0 {
            format!("{} and {more} more", titles.join(", "))
        } else {
            titles.join(", ")
        };
        Err(format!("no issue named {asked}; issues: {holds}"))
    }

    /// How a refusal names the issue `id`: its number and title, or the id
    /// as written when no part held here keeps it.
    fn reads_as(&self, id: &Id) -> String {
        self.get(id).map_or_else(
            || id.as_str().to_owned(),
            |issue| format!("#{} {}", issue.number, issue.title),
        )
    }

    /// The issue numbered `number`, if one is.
    fn issue_number(&self, number: u64) -> Option<(&Id, &Issue)> {
        self.0
            .iter()
            .rev()
            .find_map(|(_, part)| part.issues.iter().find(|(_, issue)| issue.number == number))
    }
}

/// The item an address names, as written: the last segment of an item
/// resource id (`<kind>/<field>/<id>`) or of an item's stable address
/// (`<kind>.<field>:<id>`).
fn reference_parts(reference: &str) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(last) = reference.rsplit('/').next()
        && last != reference
    {
        parts.push(last.to_owned());
    }
    if let Some(form) = ui::tilo_actions::item_form_of(reference)
        && let Some(last) = form.rsplit('/').next()
    {
        parts.push(last.to_owned());
    }
    parts
}

/// The issue `action` names in its own parameter, to resolve where every
/// issue is held.
fn item_of_mut(action: &mut IssueListAction) -> Option<&mut Id> {
    use issue_list as verb;
    match action {
        IssueListAction::Rename(verb::Rename { issue, .. })
        | IssueListAction::MoveUnder(verb::MoveUnder { issue, .. })
        | IssueListAction::Assign(verb::Assign { issue, .. })
        | IssueListAction::SetProject(verb::SetProject { issue, .. })
        | IssueListAction::Attach(verb::Attach { issue, .. })
        | IssueListAction::Detach(verb::Detach { issue, .. })
        | IssueListAction::AddLabel(verb::AddLabel { issue, .. })
        | IssueListAction::Place(verb::Place { issue, .. })
        | IssueListAction::Comment(verb::Comment { issue, .. })
        | IssueListAction::Link(verb::Link { issue, .. })
        | IssueListAction::LinkNote(verb::LinkNote { issue, .. })
        | IssueListAction::Delete(verb::Delete { issue })
        | IssueListAction::Plan(verb::Plan { issue, .. })
        | IssueListAction::SetPlace(verb::SetPlace { issue, .. })
        | IssueListAction::Wait(verb::Wait { issue, .. })
        | IssueListAction::Resume(verb::Resume { issue, .. })
        | IssueListAction::SetBudget(verb::SetBudget { issue, .. })
        | IssueListAction::SetPiecesAtOnce(verb::SetPiecesAtOnce { issue, .. })
        | IssueListAction::SessionBegan(verb::SessionBegan { issue })
        | IssueListAction::SessionEnded(verb::SessionEnded { issue, .. })
        | IssueListAction::Stop(verb::Stop { issue, .. }) => Some(issue),
        _ => None,
    }
}

mod listing;
mod many;

#[cfg(test)]
pub(crate) mod tests;

/// The chain the parts belong to, beside the tracker's own document.
#[must_use]
pub fn chain() -> ui::resource::DocumentId {
    ui::resource::DocumentId::derived("issues", &ui::resource::DocumentId::workspace_default())
}

impl ui::framework::Chained for IssueList {
    type Base = Tracker;

    fn chain() -> ui::resource::DocumentId {
        chain()
    }

    fn prepare(
        actions: &mut [Self::Action],
        tracker: &Tracker,
        parts: &[(u64, Rc<Self>)],
        _invoker: &ui::tilo_actions::ActionInvoker,
    ) -> Result<(), String> {
        prepare(actions, tracker, parts)
    }

    fn carry(_previous: u64, part: &Self) -> Option<tilo_resource::Invoke<Self>> {
        Some(Self::START.with(issue_list::Start {
            before: carried(part),
        }))
    }

    /// The list and the board show every issue, so a window holds every
    /// part; a part bounds what one change costs, not what is held.
    const HELD_WHOLE: bool = true;

    fn prepare_read(
        filter: &mut serde_json::Map<String, serde_json::Value>,
        tracker: &Tracker,
        parts: &[(u64, Rc<Self>)],
    ) -> Result<(), String> {
        listing::prepare_read(filter, tracker, parts)
    }
}
