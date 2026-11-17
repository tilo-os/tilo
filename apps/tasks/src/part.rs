//! The tracker's issues, kept in numbered parts beside the tracker.
//!
//! One record cannot hold every issue a workspace ever wrote at a cost that
//! stays flat: every reader of a record loads, verifies and diffs all of it,
//! so a change to one issue of thousands would cost thousands. The issues
//! live in a chain of [`IssuePart`] records instead (`tilo_app`'s
//! `Chained`), each holding up to [`PART_ISSUES`] issues or
//! [`PART_TEXT_BYTES`] of their text; a change touches the one part that
//! holds its issue. The tracker's own record ([`crate::model::Tracker`])
//! keeps the projects and areas above them.
//!
//! - **Where a new issue goes.** To the newest part, unless it is full
//!   ([`new_part`]): then to the next. Two writers starting the same part
//!   start the same record, so their issues meet there.
//! - **Where a change goes.** To the part holding the issue it names.
//! - **Numbers.** A part counts the issues made in it on top of the highest
//!   number given before it started ([`IssuePart::before`]), which whoever
//!   starts it carries over from the part before.
//! - **Order.** Each issue carries its place in the tracker's order
//!   ([`Issue::rank`]), set where it is made or placed from every issue held
//!   there ([`Issues`]), so the list and the board read across parts.
//! - **What reads across parts.** A sub-issue whose parent is gone stands at
//!   the top level, and an issue whose project was deleted works towards
//!   none: read so, never rewritten in other parts ([`Issues`]).

use std::collections::HashMap;
use std::rc::Rc;

use tilo_document::RichText;
use tilo_reference::ResourceAt;
use ui::tilo_resource::items::Ref;
use ui::tilo_resource::{
    self, ActionCx, Bytes, Collection, Counter, Id, Model, NonBlank, Origin, Refusal, resource,
};

use crate::model::{
    Area, Attachment, Issue, Link, MAX_ESTIMATE, MAX_MESSAGE_CHARS, Message, NewIssue, Priority,
    Status, Tracker, checked_assignee,
};

/// Issues a part holds before the next one starts.
pub const PART_ISSUES: usize = 500;

/// Text, in bytes, a part's issues hold (titles, descriptions, threads)
/// before the next one starts: what keeps one part, and so one change, cheap
/// whatever its issues say.
pub const PART_TEXT_BYTES: usize = 512 * 1024;

/// One part of the tracker's issues: every issue made while it was the
/// newest, and everything done to them since.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct IssuePart {
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

impl IssuePart {
    /// The highest number this part gave, or the one before it started.
    #[must_use]
    pub fn last_number(&self) -> u64 {
        self.before
            .saturating_add(u64::try_from(self.numbers.get()).unwrap_or_default())
    }

    /// The text its issues hold, in bytes: titles, descriptions and threads.
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        self.issues
            .values()
            .map(|issue| {
                issue
                    .comments
                    .values()
                    .map(|message| message.text.len())
                    .sum::<usize>()
                    .saturating_add(issue.title.len())
                    .saturating_add(issue.body.text().len())
            })
            .sum()
    }

    fn issue_mut(&mut self, issue: &Id) -> Result<&mut Issue, Refusal> {
        self.issues.get_mut(issue).ok_or_else(no_issue)
    }

    fn next_number(&mut self) -> u64 {
        self.numbers.add(1);
        self.last_number()
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

/// The part a new issue goes to, given the newest part held (its number and
/// state): that part, or the next once it is full.
#[must_use]
pub fn new_part(head: Option<(u64, &IssuePart)>) -> u64 {
    match head {
        None => 0,
        Some((index, part))
            if part.issues.len() >= PART_ISSUES || part.text_bytes() >= PART_TEXT_BYTES =>
        {
            index.saturating_add(1)
        }
        Some((index, _)) => index,
    }
}

/// What part `part` hands the part after it:
/// the number it starts counting from.
#[must_use]
pub fn carried(part: &IssuePart) -> u64 {
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

fn message_text(text: NonBlank) -> Result<String, Refusal> {
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(Refusal::new("This message is too long"));
    }
    Ok(text.into_string())
}

#[resource(
    kind = "tasks.issues",
    params = issue_part,
    search = IssuePart::search,
    effects = crate::model::TasksEffect,
    read_content = IssuePart::read_document
)]
impl IssuePart {
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
        self.issues.push(id, issue);
        Ok(())
    }

    /// Start counting this part's numbers on from `before`, the highest
    /// number the part before it gave. Written once, as the part starts.
    #[action(step = "create", housekeeping, writes = issues)]
    pub fn start(&mut self, before: u64) {
        self.before = self.before.max(before);
    }

    /// Rename an issue: its title, as the board and its page show it.
    #[action(item = issues, field = "title", step = "edit", writes = issues)]
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
    /// the agent works on it as their turn and answers in its thread.
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
        let (by, at_ms) = given_to_agent(assignee.as_ref(), Some(&author(cx)?), cx.now_ms());
        let target = self.issue_mut(issue)?;
        // Giving it to the agent it is with already asks nothing new.
        if target.assignee != assignee || by.is_none() {
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
    #[action(item = issues, step = "edit", writes = issues)]
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
    #[action(item = issues, step = "edit", audience = "people", writes = issues)]
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
}

/// Every issue the held parts keep, read as one tracker: in the tracker's
/// order, each found by its id, with what reads across parts (a parent that
/// is gone, a project that was deleted) resolved the same way everywhere.
///
/// Built from the parts once per change of any of them, and read by the
/// list, the board, an issue's page and the checks an action makes where it
/// is invoked.
pub struct Issues<'a> {
    /// Every issue in the tracker's order: by rank, then number, then id.
    ordered: Vec<(&'a Id, &'a Issue)>,
    /// Where each issue is in `ordered`, and the part that keeps it.
    at: HashMap<&'a Id, (usize, u64)>,
    /// The issues standing at the top, and those under each parent, by
    /// where they are in `ordered`: read once here, so walking the tree asks
    /// each issue for its sub-issues without reading every issue again.
    top: Vec<usize>,
    under: HashMap<&'a Id, Vec<usize>>,
}

impl<'a> Issues<'a> {
    /// The issues `parts` keep, oldest part first. An issue two parts both
    /// claim (never written so, but read safely) is the newer part's.
    #[must_use]
    pub fn of(parts: &'a [(u64, Rc<IssuePart>)]) -> Self {
        let mut found: HashMap<&'a Id, (&'a Issue, u64)> = HashMap::new();
        for (index, part) in parts {
            for (id, issue) in part.issues.iter() {
                found.insert(id, (issue, *index));
            }
        }
        let mut ordered: Vec<(&'a Id, &'a Issue)> =
            found.iter().map(|(id, (issue, _))| (*id, *issue)).collect();
        ordered.sort_by(|(a_id, a), (b_id, b)| {
            a.rank
                .cmp(&b.rank)
                .then(a.number.cmp(&b.number))
                .then_with(|| a_id.cmp(b_id))
        });
        let at: HashMap<&'a Id, (usize, u64)> = ordered
            .iter()
            .enumerate()
            .filter_map(|(position, (id, _))| Some((*id, (position, found.get(id)?.1))))
            .collect();
        let mut top = Vec::new();
        let mut under: HashMap<&'a Id, Vec<usize>> = HashMap::new();
        for (position, (_, issue)) in ordered.iter().enumerate() {
            // A parent that is gone leaves its sub-issue at the top.
            match issue
                .parent
                .as_ref()
                .and_then(|parent| at.get_key_value(parent))
            {
                Some((parent, _)) => under.entry(*parent).or_default().push(position),
                None => top.push(position),
            }
        }
        Self {
            ordered,
            at,
            top,
            under,
        }
    }

    /// How many issues there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ordered.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    /// Every issue, in the tracker's order.
    pub fn iter(&self) -> impl Iterator<Item = (&'a Id, &'a Issue)> + '_ {
        self.ordered.iter().copied()
    }

    /// The issue `id`, if one is.
    #[must_use]
    pub fn get(&self, id: &Id) -> Option<&'a Issue> {
        self.at
            .get(id)
            .and_then(|(position, _)| self.ordered.get(*position))
            .map(|(_, issue)| *issue)
    }

    /// Whether `id` is an issue here.
    #[must_use]
    pub fn contains(&self, id: &Id) -> bool {
        self.at.contains_key(id)
    }

    /// The part that keeps the issue `id`.
    #[must_use]
    pub fn part_of(&self, id: &Id) -> Option<u64> {
        self.at.get(id).map(|(_, part)| *part)
    }

    /// The issue numbered `number`, if one is.
    #[must_use]
    pub fn by_number(&self, number: u64) -> Option<(&'a Id, &'a Issue)> {
        self.iter().find(|(_, issue)| issue.number == number)
    }

    /// The parent `issue` stands under: the one it names, while that one is
    /// here.
    #[must_use]
    pub fn parent_of(&self, issue: &Issue) -> Option<&'a Id> {
        issue
            .parent
            .as_ref()
            .and_then(|parent| self.at.get_key_value(parent).map(|(id, _)| *id))
    }

    /// The direct sub-issues of `parent` (top-level issues for `None`), in
    /// the tracker's order.
    pub fn children<'s>(
        &'s self,
        parent: Option<&'s Id>,
    ) -> impl Iterator<Item = (&'a Id, &'a Issue)> + 's {
        let positions = match parent {
            None => self.top.as_slice(),
            Some(parent) => self.under.get(parent).map_or(&[][..], Vec::as_slice),
        };
        positions
            .iter()
            .filter_map(|position| self.ordered.get(*position).copied())
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
        for _ in 0..=self.ordered.len() {
            match at {
                Some(id) if id == ancestor => return true,
                Some(id) => at = self.get(id).and_then(|issue| self.parent_of(issue)),
                None => return false,
            }
        }
        false
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
    pub fn in_area<'s>(&'s self, area: &'s Id) -> impl Iterator<Item = (&'a Id, &'a Issue)> + 's {
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
struct Held<'a>(&'a [(u64, Rc<IssuePart>)]);

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

/// Fill in, where `action` is invoked, what it reads of the tracker's
/// projects and of every issue held there, and refuse what they do not
/// allow: a parent that is not an issue, a project the tracker does not
/// have, an issue placed under its own sub-issue.
///
/// # Errors
/// Refuses as the action's verb would, had it every part.
pub fn prepare(
    action: &mut <IssuePart as tilo_resource::Resource>::Action,
    tracker: &Tracker,
    parts: &[(u64, Rc<IssuePart>)],
) -> Result<(), String> {
    use issue_part::{Create, MoveUnder, Place, SetProject};
    let held = Held(parts);
    // The issue an action names in its own parameter may be written as its
    // title, its number or an address too: it is resolved here, where every
    // part is held, so the part keeping it can be found.
    if let Some(issue) = item_of_mut(&mut action.operation)
        && held.get(issue).is_none()
    {
        *issue = held.named(issue.as_str())?;
    }
    let check_parent = |parent: Option<&Ref>| -> Result<Option<Id>, String> {
        parent.map(|parent| held.named(parent.as_str())).transpose()
    };
    let goal = |project: Option<&Ref>| -> Result<Option<Area>, String> {
        project
            .map(|project| {
                let area = tracker
                    .area_named(project.as_str())
                    .map_err(|refusal| refusal.to_string())?;
                let named = tracker
                    .area(&area)
                    .ok_or_else(|| Tracker::no_area().to_string())?;
                Ok(crate::model::area_goal(&area, &named.name))
            })
            .transpose()
    };
    match &mut action.operation {
        IssuePartAction::Create(Create { issue }) => {
            if let Some(parent) = issue.parent.as_mut()
                && let Some(id) = check_parent(Some(parent))?
            {
                *parent = Ref::from(id);
            }
            issue.goal = goal(issue.project.as_ref())?;
            issue.rank = Some(held.last_rank());
        }
        IssuePartAction::MoveUnder(MoveUnder { issue, parent }) => {
            let resolved = check_parent(parent.as_ref())?;
            if let Some(parent) = &resolved
                && held.is_under(parent, issue)
            {
                return Err("An issue cannot sit under itself or its own sub-issues".into());
            }
            *parent = resolved.map(Ref::from);
        }
        IssuePartAction::SetProject(SetProject {
            project, goal: to, ..
        }) => {
            *to = goal(project.as_ref())?;
        }
        IssuePartAction::Place(Place {
            issue,
            before,
            rank,
            ..
        }) => {
            let named_before = check_parent(before.as_ref())?;
            *rank = Some(
                match named_before.as_ref().filter(|before| *before != issue) {
                    Some(before) => held.rank_before(before, issue),
                    None => held.last_rank(),
                },
            );
            *before = named_before.map(Ref::from);
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
    if let Some(form) = tilo_resource::items::item_form_of(reference)
        && let Some(last) = form.rsplit('/').next()
    {
        parts.push(last.to_owned());
    }
    parts
}

/// The part `action` changes, given the parts held, oldest first: the one
/// holding the issue it names, or for a new issue the newest part (or the
/// next, once that is full).
///
/// # Errors
/// Refuses an action naming an issue no part held here keeps.
pub fn part_for(
    action: &<IssuePart as tilo_resource::Resource>::Action,
    parts: &[(u64, Rc<IssuePart>)],
) -> Result<u64, String> {
    let holding = |issue: &Id| {
        parts
            .iter()
            .rev()
            .find(|(_, part)| part.issues.contains(issue))
            .map(|(index, _)| *index)
            .ok_or_else(|| no_issue().to_string())
    };
    let newest = || new_part(parts.last().map(|(index, part)| (*index, &**part)));
    match &action.operation {
        IssuePartAction::Create(_) => Ok(newest()),
        other => match item_of(other) {
            Some(issue) => holding(issue),
            None => Ok(parts.last().map_or(0, |(index, _)| *index)),
        },
    }
}

/// The issue a verb on one issue names.
#[must_use]
/// The issue `action` names in its own parameter, to resolve where every
/// part is held ([`item_of`] for the id it then names).
fn item_of_mut(action: &mut IssuePartAction) -> Option<&mut Id> {
    use issue_part as verb;
    match action {
        IssuePartAction::Rename(verb::Rename { issue, .. })
        | IssuePartAction::MoveUnder(verb::MoveUnder { issue, .. })
        | IssuePartAction::Assign(verb::Assign { issue, .. })
        | IssuePartAction::SetProject(verb::SetProject { issue, .. })
        | IssuePartAction::Attach(verb::Attach { issue, .. })
        | IssuePartAction::Detach(verb::Detach { issue, .. })
        | IssuePartAction::AddLabel(verb::AddLabel { issue, .. })
        | IssuePartAction::Place(verb::Place { issue, .. })
        | IssuePartAction::Comment(verb::Comment { issue, .. })
        | IssuePartAction::Link(verb::Link { issue, .. })
        | IssuePartAction::LinkNote(verb::LinkNote { issue, .. })
        | IssuePartAction::Delete(verb::Delete { issue })
        | IssuePartAction::Plan(verb::Plan { issue, .. }) => Some(issue),
        _ => None,
    }
}

pub fn item_of(action: &IssuePartAction) -> Option<&Id> {
    use issue_part as verb;
    match action {
        IssuePartAction::Rename(verb::Rename { issue, .. })
        | IssuePartAction::MoveUnder(verb::MoveUnder { issue, .. })
        | IssuePartAction::Assign(verb::Assign { issue, .. })
        | IssuePartAction::SetProject(verb::SetProject { issue, .. })
        | IssuePartAction::Attach(verb::Attach { issue, .. })
        | IssuePartAction::Detach(verb::Detach { issue, .. })
        | IssuePartAction::AddLabel(verb::AddLabel { issue, .. })
        | IssuePartAction::Place(verb::Place { issue, .. })
        | IssuePartAction::Comment(verb::Comment { issue, .. })
        | IssuePartAction::Link(verb::Link { issue, .. })
        | IssuePartAction::LinkNote(verb::LinkNote { issue, .. })
        | IssuePartAction::Delete(verb::Delete { issue })
        | IssuePartAction::Plan(verb::Plan { issue, .. }) => Some(issue),
        // A field edit of one issue (its title, its description) names it
        // in its path.
        IssuePartAction::FieldEdit(edit) => match edit.path() {
            [
                tilo_resource::Segment::Field(field),
                tilo_resource::Segment::Item(issue),
                ..,
            ] if field == "issues" => Some(issue),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests;

/// The chain the parts belong to, beside the tracker's own record.
#[must_use]
pub fn chain() -> ui::resource::DocumentId {
    ui::resource::DocumentId::derived("issues", &ui::resource::DocumentId::workspace_default())
}

impl ui::framework::Chained for IssuePart {
    type Base = Tracker;

    fn chain() -> ui::resource::DocumentId {
        chain()
    }

    fn part_for(action: &Self::Action, parts: &[(u64, Rc<Self>)]) -> Result<u64, String> {
        part_for(action, parts)
    }

    fn prepare(
        action: &mut Self::Action,
        tracker: &Tracker,
        parts: &[(u64, Rc<Self>)],
    ) -> Result<(), String> {
        prepare(action, tracker, parts)
    }

    fn carry(_previous: u64, part: &Self) -> Option<tilo_resource::Invoke<Self>> {
        Some(Self::START.with(issue_part::Start {
            before: carried(part),
        }))
    }

    /// The list and the board show every issue, so a window holds every
    /// part; a part bounds what one change costs, not what is held.
    const HELD_WHOLE: bool = true;
}
