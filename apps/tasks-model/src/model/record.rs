//! The tracker: every issue of the workspace, and the verbs on them.

use tilo_document::RichText;
use tilo_reference::ResourceAt;
use tilo_resource::{
    self, ActionCx, Bytes, Collection, Counter, Id, Model, NonBlank, Origin, Refusal, Set, resource,
};

use crate::model::goal::Area;
use crate::model::issue::{
    Attachment, Imported, Issue, Link, MAX_ATTACHMENT_NAME_CHARS, MAX_ESTIMATE, MAX_MESSAGE_CHARS,
    Message, NewIssue, Priority, Status, checked_assignee,
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
    /// Tasks of the workspace already imported, by their id: an
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

    /// What an issue that works towards the project or area `area` names as
    /// its goal; refused when the tracker has no such project.
    fn goal_of(&self, area: &Id) -> Result<Area, Refusal> {
        let named = self.areas.get(area).ok_or_else(Self::no_area)?;
        Ok(crate::model::area_goal(area, &named.name))
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

fn author(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .map(str::to_owned)
        .ok_or_else(|| Refusal::new("Sign in to do this"))
}

fn check_estimate(estimate: Option<u32>) -> Result<(), Refusal> {
    if estimate.is_some_and(|estimate| estimate > MAX_ESTIMATE) {
        return Err(Refusal::new("An estimate is at most 1000 points"));
    }
    Ok(())
}

fn by_agent(cx: &ActionCx) -> bool {
    matches!(cx.actor_kind(), Some(tilo_actions::ActorKind::Agent))
}

fn message_text(text: NonBlank) -> Result<String, Refusal> {
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(Refusal::new("This message is too long"));
    }
    Ok(text.into_string())
}

fn push_message(issue: &mut Issue, cx: &mut ActionCx, message: Message) {
    let id = cx.id();
    issue.comments.push(id, message);
}

impl Tracker {
    /// What the record holds: the issues, in the order the board shows them, and the projects
    /// and areas above them.
    ///
    /// It answers through [`fields`](tilo_resource::read::fields): a caller
    /// with no window open reads the record as it reads any app's record, and
    /// a named item is read from the record's table or by the read its own
    /// type gives it.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "tasks.tracker",
    params = tracker,
    search = crate::model::search::entries,
    effects = crate::model::TasksEffect,
    read_content = Tracker::read_document
)]
impl Tracker {
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
            project,
            priority,
            estimate,
            due_ms,
            labels,
            origin,
        } = issue;
        self.check_parent(parent.as_ref())?;
        let area = project
            .as_ref()
            .map(|project| self.goal_of(project))
            .transpose()?;
        check_estimate(estimate)?;
        let origin = Origin::checked_option(origin)?;
        let assignee = checked_assignee(assignee)?;
        let author = author(cx)?;
        let author_name = tilo_resource::acting_name(cx);
        let number = self.next_number();
        let mut kept_labels = tilo_resource::Set::new();
        for label in labels.unwrap_or_default() {
            let label = label.trim();
            if !label.is_empty() {
                kept_labels.insert(label.to_owned());
            }
        }
        let issue = Issue {
            number,
            title: title.into_string(),
            body: RichText::from_markdown(body.unwrap_or_default()),
            status: status.unwrap_or_default(),
            assignee,
            url: url
                .map(|url| url.trim().to_owned())
                .filter(|url| !url.is_empty())
                .or_else(|| origin.as_ref().and_then(|origin| origin.url.clone())),
            parent,
            author,
            author_name,
            created_ms: cx.now_ms(),
            area,
            priority: priority.and_then(Priority::level),
            estimate,
            due_ms,
            labels: kept_labels,
            origin,
            ..Issue::default()
        };
        let id = cx.id();
        self.issues.push(id, issue);
        Ok(())
    }

    /// Create a project or an area: the resource above the issues written
    /// for it. It starts with nobody involved and holds nothing of its own
    /// yet; tasks join it by working towards it.
    ///
    /// # Errors
    /// Refuses a name too long, or an id already taken.
    #[action(step = "create", writes = areas)]
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

    /// Rename an issue: its title, as the board and its page show it.
    #[action(item = issues, field = "title", step = "edit", writes = issues)]
    pub fn rename(&mut self, issue: &Id, title: NonBlank) -> Result<(), Refusal> {
        let title = title.into_string();
        if title.chars().count() > crate::model::issue::MAX_TITLE_CHARS {
            return Err(Refusal::new("An issue's title fits on a line"));
        }
        title.trim().clone_into(&mut self.issue_mut(issue)?.title);
        Ok(())
    }

    /// Rename the project or area `area`.
    #[action(item = areas, field = "name", step = "edit", writes = areas)]
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
    #[action(item = areas, step = "edit", writes = areas)]
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
    #[action(item = areas, step = "create", writes = areas)]
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
    #[action(item = areas, step = "delete", writes = areas)]
    pub fn remove_person(&mut self, area: &Id, person: &Id) -> Result<(), Refusal> {
        self.area_mut(area)?.people.remove(person);
        Ok(())
    }

    /// Move an issue under another one, or to the top level.
    #[action(item = issues, step = "move", writes = issues)]
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

    /// Assign an issue to a person or an agent, to someone not in the
    /// workspace yet by their name, or to nobody.
    #[action(item = issues, step = "edit", writes = issues)]
    pub fn assign(
        &mut self,
        issue: &Id,
        #[param(
            "person:<id>, agent:<id>, or outside:<name> for someone not in the workspace yet; nobody without"
        )]
        assignee: Option<String>,
    ) -> Result<(), Refusal> {
        let assignee = checked_assignee(assignee)?;
        self.issue_mut(issue)?.assignee = assignee;
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
        #[param("The project or area it works towards, by its id; none without")] project: Option<
            Id,
        >,
    ) -> Result<(), Refusal> {
        let area = project
            .as_ref()
            .map(|project| self.goal_of(project))
            .transpose()?;
        self.issue_mut(issue)?.area = area;
        Ok(())
    }

    /// Delete a project or an area. Its issues stay, working towards no
    /// project.
    #[action(item = areas, step = "delete", destructive, writes = areas, writes = issues)]
    pub fn delete_area(&mut self, area: &Id) -> Result<(), Refusal> {
        self.areas.remove(area).ok_or_else(Self::no_area)?;
        let holding: Vec<Id> = self.in_area(area).map(|(id, _)| id.clone()).collect();
        for issue in holding {
            if let Some(issue) = self.issues.get_mut(&issue) {
                issue.area = None;
            }
        }
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
        if name.chars().count() > MAX_ATTACHMENT_NAME_CHARS {
            return Err(Refusal::new("An attachment's name fits on a line"));
        }
        let author = author(cx)?;
        let author_name = tilo_resource::acting_name(cx);
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
        let author = author(cx)?;
        let author_name = tilo_resource::acting_name(cx);
        let agent = by_agent(cx);
        let at_ms = cx.now_ms();
        let target = self.issue_mut(issue)?;
        push_message(
            target,
            cx,
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

    /// Delete an issue. Its sub-issues move up to its parent.
    #[action(item = issues, step = "delete", destructive, writes = issues)]
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

    /// Import tasks the workspace kept before the tracker. A task
    /// already imported is left alone.
    #[action(step = "create", housekeeping, writes = issues)]
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
