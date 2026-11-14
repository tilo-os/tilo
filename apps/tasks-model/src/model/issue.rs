//! One issue and what it holds: its thread and its links.

use serde::{Deserialize, Serialize};
use tilo_document::RichText;
use tilo_resource::{
    self, Atom, Bytes, Collection, Id, JsonSchema, Model, NonBlank, Origin, Refusal, Set,
};

use crate::model::goal::Area;

/// The longest title an issue keeps.
pub const MAX_TITLE_CHARS: usize = 240;
/// The longest message a thread keeps.
pub const MAX_MESSAGE_CHARS: usize = 16 * 1024;
/// The most labels one issue carries.
pub const MAX_LABELS: usize = 32;
/// The largest estimate an issue takes, in points.
pub const MAX_ESTIMATE: u32 = 1000;
/// The longest name an attachment keeps.
pub const MAX_ATTACHMENT_NAME_CHARS: usize = 255;

/// How urgent an issue is, by name: what an action takes for its
/// [`Issue::priority`], kept as its level from 1 (urgent) to 4 (low).
#[derive(
    Model, Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Priority {
    /// Nobody ranked it.
    #[default]
    None,
    Urgent,
    High,
    Medium,
    Low,
}

impl Priority {
    /// The level the issue keeps: 1 (urgent) to 4 (low), none for
    /// [`Priority::None`].
    #[must_use]
    pub const fn level(self) -> Option<u8> {
        match self {
            Self::None => None,
            Self::Urgent => Some(1),
            Self::High => Some(2),
            Self::Medium => Some(3),
            Self::Low => Some(4),
        }
    }

    /// The priority of the kept level `level`.
    #[must_use]
    pub const fn of_level(level: Option<u8>) -> Self {
        match level {
            Some(1) => Self::Urgent,
            Some(2) => Self::High,
            Some(3) => Self::Medium,
            Some(4) => Self::Low,
            _ => Self::None,
        }
    }

    /// The Fluent id naming the priority.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::None => "tasks-priority-none",
            Self::Urgent => "tasks-priority-urgent",
            Self::High => "tasks-priority-high",
            Self::Medium => "tasks-priority-medium",
            Self::Low => "tasks-priority-low",
        }
    }
}

/// A new issue: what `create` takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NewIssue {
    /// What the issue is about, in a line
    pub title: NonBlank,
    /// The issue it is a sub-issue of
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Id>,
    /// Its description, in markdown
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Where it starts; Backlog without
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    /// person:<id>, agent:<id>, or outside:<name> for someone not in the
    /// workspace yet
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// The page it is read at elsewhere, when it was written from one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The project or area it works towards, by its id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Id>,
    /// How urgent it is: none, urgent, high, medium or low
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// How much work it is, in points
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<u32>,
    /// When it is wanted, in milliseconds since the epoch
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_ms: Option<i64>,
    /// Labels it carries
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Where it was first written, when it is moved here from elsewhere:
    /// shown as its author and date, while its history keeps who moved it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

impl NewIssue {
    /// A new issue titled `title`, every other argument left out.
    #[must_use]
    pub fn titled(title: NonBlank) -> Self {
        Self {
            title,
            parent: None,
            body: None,
            status: None,
            assignee: None,
            url: None,
            project: None,
            priority: None,
            estimate: None,
            due_ms: None,
            labels: None,
            origin: None,
        }
    }
}

/// A file attached to an issue: a screenshot, a log, a document it was
/// written from.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Attachment {
    /// Its file name.
    #[model(label, readonly)]
    pub name: String,
    /// Its media type (`image/png`), when known.
    #[model(readonly)]
    pub media_type: Option<String>,
    /// Its bytes, kept with the workspace's content.
    #[model(readonly)]
    pub bytes: Bytes,
    /// Who attached it.
    #[model(readonly)]
    pub author: String,
    /// The name its author acted under, when software attached it.
    #[model(readonly)]
    pub author_name: String,
    #[model(readonly)]
    pub at_ms: i64,
}

/// Where an issue is: one column of the board each.
#[derive(
    Model,
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Written down, not planned yet.
    #[default]
    Backlog,
    /// Planned: the next thing to pick up.
    Todo,
    /// Someone is working on it.
    InProgress,
    /// Done by whoever worked on it, waiting for a person to look.
    InReview,
    Done,
    Cancelled,
}

impl Status {
    /// Every status, in the board's column order.
    pub const ALL: [Self; 6] = [
        Self::Backlog,
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::Done,
        Self::Cancelled,
    ];

    /// Whether the work is over, one way or the other.
    #[must_use]
    pub const fn closed(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    /// Whether the issue has left where a task nobody has started waits:
    /// someone has taken it up. A to-do is an issue that has not.
    #[must_use]
    pub const fn started(self) -> bool {
        !matches!(self, Self::Backlog | Self::Todo)
    }

    /// The Fluent id naming the status.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Backlog => "tasks-status-backlog",
            Self::Todo => "tasks-status-todo",
            Self::InProgress => "tasks-status-in-progress",
            Self::InReview => "tasks-status-in-review",
            Self::Done => "tasks-status-done",
            Self::Cancelled => "tasks-status-cancelled",
        }
    }
}

/// What a message in an issue's thread is.
///
/// `ToRun`, `FromRun`, `Lookup`, `Step` and `Answer` are what an agent's task
/// runs posted here, and stay so that a thread written while they ran still
/// reads as it did. Nothing writes them now: an agent takes part in a
/// workspace conversation and its work is that conversation.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// Someone said something about the issue.
    #[default]
    Comment,
    /// A message to an earlier task run.
    ToRun,
    /// What a task run did or found, posted by its agent.
    FromRun,
    /// A resource a task run read, looked up through system search.
    Lookup,
    /// A step a task run took: a tilolang program it ran and what it
    /// answered, in [`Message::step`].
    Step,
    /// A task run's answer as a view, in [`Message::lookup`].
    Answer,
}

/// One message of an issue's thread.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Message {
    /// Who wrote it: a person, or an agent.
    #[model(readonly)]
    pub author: String,
    /// The name its author acted under, when software wrote it: an
    /// agent's name, shown where the workspace no longer names the agent.
    #[model(readonly)]
    pub author_name: String,
    /// Whether an agent wrote it.
    #[model(readonly)]
    pub by_agent: bool,
    #[model(readonly)]
    pub at_ms: i64,
    /// Where it was first written, when it was moved here from elsewhere:
    /// shown as its author and date, while `author` and `at_ms` keep who
    /// moved it and when.
    #[model(readonly)]
    pub origin: Option<Origin>,
    /// Text
    ///
    /// Markdown, as it was written.
    #[model(readonly)]
    pub text: String,
    #[model(readonly)]
    pub kind: MessageKind,
    /// The run it is to or from; 0 for a comment.
    #[model(readonly)]
    pub run: u64,
    /// The resource a [`MessageKind::Lookup`] read, or the view a
    /// [`MessageKind::Answer`] keeps: its address in the app that keeps it.
    /// Empty for any other message.
    #[model(readonly)]
    pub lookup: String,
    /// The step a [`MessageKind::Step`] took, as its
    /// `tilo_reference::ProgramStep` JSON. Empty for any other message.
    #[model(readonly)]
    pub step: String,
    /// A message to a run that its agent took in.
    #[model(readonly)]
    pub incorporated: bool,
}

/// A link from an issue to another resource: a file, a document, a message.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Link {
    /// How the link reads; the resource it names decides what it is.
    #[model(label)]
    pub label: String,
    /// The app that keeps the resource.
    pub app: String,
    /// The resource's kind.
    pub kind: String,
    /// The resource's address in its app.
    pub resource: String,
    /// The app's record that keeps it, such as a page kept on its own. None
    /// for the app's own record of the workspace, which every link made
    /// before links named a record means.
    pub record: Option<String>,
}

/// One issue.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Issue::valid)]
pub struct Issue {
    /// Number
    ///
    /// How the issue reads in a reference (`#12`), from the tracker's
    /// counter when it was made.
    #[model(readonly)]
    pub number: u64,
    /// Title
    #[model(label)]
    pub title: String,
    /// Description
    ///
    /// Markdown, edited in the block editor. Read whole: search, references
    /// and an agent's instruction read every word of it.
    #[model(whole)]
    #[model(resource)]
    pub body: RichText,
    /// Status
    pub status: Status,
    /// Assignee
    ///
    /// Whom the issue is assigned to: `person:<id>`, `agent:<id>`, or
    /// `outside:<name>` for someone who has not joined the workspace yet
    /// ([`person_assignee`], [`agent_assignee`], [`outside_assignee`]).
    /// Written whole, so two
    /// people assigning at once keep one of their choices. Only `assign`
    /// changes it.
    #[model(readonly)]
    pub assignee: Option<Atom>,
    /// Labels
    pub labels: Set<String>,
    /// Parent
    ///
    /// The issue this one is a sub-issue of. Only `move-under` changes it,
    /// so an issue never ends up under itself.
    #[model(readonly)]
    pub parent: Option<Id>,
    /// Links
    pub links: Collection<Link>,
    /// Goal
    ///
    /// The project or area it works towards: an ordinary resource of the
    /// workspace (`notes.note`, `tasks.tracker/issues/7`), never state of
    /// its own. A sub-issue that names none works towards its parent's.
    #[model(readonly)]
    pub area: Option<Area>,
    /// Due
    ///
    /// When it is wanted, in milliseconds since the epoch. None for an issue
    /// nobody has dated.
    pub due_ms: Option<i64>,
    /// Priority
    ///
    /// How urgent it is, from 1 (urgent) to 4 (low); None for one nobody
    /// ranked. Actions take it by name ([`Priority`]).
    pub priority: Option<u8>,
    /// Estimate
    ///
    /// How much work it is, in points; None for one nobody estimated.
    pub estimate: Option<u32>,
    /// Attachments
    ///
    /// Files attached to it. Only `attach` and `detach` change them.
    #[model(readonly)]
    pub attachments: Collection<Attachment>,
    /// Thread
    ///
    /// Named `comments`, so whoever may comment in the workspace may write
    /// in it, and nothing else of the issue. Only its verbs add to it, so
    /// nobody edits or removes someone else's message by a field edit.
    #[model(readonly)]
    pub comments: Collection<Message>,
    /// Author
    #[model(readonly)]
    pub author: String,
    /// The name its author acted under, when software wrote it.
    #[model(readonly)]
    pub author_name: String,
    #[model(readonly)]
    pub created_ms: i64,
    /// Origin
    ///
    /// Where it was first written, when it was moved here from another
    /// tracker: shown as its author and date, while `author` and
    /// `created_ms` keep who moved it and when.
    #[model(readonly)]
    pub origin: Option<Origin>,
    /// Source
    ///
    /// The page the issue is read at elsewhere: the tracker or repository
    /// issue it was written from.
    pub url: Option<String>,
}

impl Issue {
    fn valid(&self) -> Result<(), Refusal> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err(Refusal::new("an issue has a title"));
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(Refusal::new("an issue's title fits on a line"));
        }
        if self.labels.len() > MAX_LABELS {
            return Err(Refusal::new("an issue carries at most 32 labels"));
        }
        if self.labels.iter().any(|label| label.trim().is_empty()) {
            return Err(Refusal::new("a label has a name"));
        }
        if let Some(assignee) = &self.assignee
            && assignee_parts(&assignee.0).is_none()
        {
            return Err(Refusal::new(ASSIGNEE_FORMS));
        }
        if self
            .estimate
            .is_some_and(|estimate| estimate > MAX_ESTIMATE)
        {
            return Err(Refusal::new("an estimate is at most 1000 points"));
        }
        if let Some(area) = &self.area
            && !area.names_a_resource()
        {
            return Err(Refusal::new("a goal is a resource of the workspace"));
        }
        if self
            .priority
            .is_some_and(|priority| !(1..=4).contains(&priority))
        {
            return Err(Refusal::new("a priority is 1 (most urgent) to 4"));
        }
        Ok(())
    }

    /// The agent the issue is assigned to, by its id (`agent:<id>`).
    #[must_use]
    pub fn assigned_agent(&self) -> Option<&str> {
        self.assignee
            .as_ref()
            .filter(|assignee| assignee.0.starts_with(AGENT_PREFIX))
            .map(|assignee| assignee.0.as_str())
    }

    /// The person the issue is assigned to.
    #[must_use]
    pub fn assigned_person(&self) -> Option<&str> {
        self.assignee
            .as_ref()
            .and_then(|assignee| assignee.0.strip_prefix(PERSON_PREFIX))
    }

    /// The name of someone outside the workspace the issue is assigned to,
    /// until they join.
    #[must_use]
    pub fn assigned_outsider(&self) -> Option<&str> {
        self.assignee
            .as_ref()
            .and_then(|assignee| assignee.0.strip_prefix(OUTSIDE_PREFIX))
    }

    /// Who the issue shows as its author: its original author when it was
    /// moved here from elsewhere, else the name its author acted under, else
    /// its author's id for a view to name.
    #[must_use]
    pub fn shown_author(&self) -> &str {
        shown_author(&self.author, &self.author_name, self.origin.as_ref())
    }

    /// When the issue shows it was written: when it was written where it
    /// came from, else when it was written here.
    #[must_use]
    pub fn shown_created_ms(&self) -> i64 {
        self.origin
            .as_ref()
            .and_then(|origin| origin.created_ms)
            .unwrap_or(self.created_ms)
    }
}

impl Message {
    /// Who the message shows as its author ([`Issue::shown_author`]).
    #[must_use]
    pub fn shown_author(&self) -> &str {
        shown_author(&self.author, &self.author_name, self.origin.as_ref())
    }

    /// When the message shows it was written ([`Issue::shown_created_ms`]).
    #[must_use]
    pub fn shown_at_ms(&self) -> i64 {
        self.origin
            .as_ref()
            .and_then(|origin| origin.created_ms)
            .unwrap_or(self.at_ms)
    }
}

fn shown_author<'a>(author: &'a str, name: &'a str, origin: Option<&'a Origin>) -> &'a str {
    if let Some(origin) = origin {
        return &origin.author;
    }
    if name.trim().is_empty() { author } else { name }
}

const PERSON_PREFIX: &str = "person:";
const AGENT_PREFIX: &str = "agent:";
const OUTSIDE_PREFIX: &str = "outside:";

/// What an assignee may be, as a refusal says it.
pub const ASSIGNEE_FORMS: &str = "an assignee is person:<id>, agent:<id>, or outside:<name> for someone not in the workspace yet";

fn assignee_parts(assignee: &str) -> Option<(&'static str, &str)> {
    let (prefix, id) = if let Some(id) = assignee.strip_prefix(PERSON_PREFIX) {
        (PERSON_PREFIX, id)
    } else if let Some(name) = assignee.strip_prefix(OUTSIDE_PREFIX) {
        (OUTSIDE_PREFIX, name)
    } else {
        (AGENT_PREFIX, assignee.strip_prefix(AGENT_PREFIX)?)
    };
    let fits = id.chars().count() <= tilo_resource::origin::MAX_ORIGIN_CHARS;
    (!id.trim().is_empty() && id.trim() == id && fits).then_some((prefix, id))
}

/// The assignee value naming someone outside the workspace, by `name`: whom
/// an issue moved from elsewhere was assigned to there, until they join.
#[must_use]
pub fn outside_assignee(name: &str) -> Atom {
    Atom(format!("{OUTSIDE_PREFIX}{}", name.trim()))
}

/// `assignee` as an issue keeps it: trimmed, none when blank.
///
/// # Errors
/// Refuses one that is not a person, an agent or an outsider by name.
pub fn checked_assignee(assignee: Option<String>) -> Result<Option<Atom>, Refusal> {
    let Some(assignee) = assignee
        .map(|assignee| assignee.trim().to_owned())
        .filter(|assignee| !assignee.is_empty())
    else {
        return Ok(None);
    };
    if assignee_parts(&assignee).is_none() {
        return Err(Refusal::new(ASSIGNEE_FORMS));
    }
    Ok(Some(Atom(assignee)))
}

/// The assignee value naming the person `id`.
#[must_use]
pub fn person_assignee(id: &str) -> Atom {
    Atom(format!("{PERSON_PREFIX}{id}"))
}

/// The assignee value naming an agent. An agent id already carries its
/// `agent:` prefix, as the action door names agents; a bare id gets it.
#[must_use]
pub fn agent_assignee(id: &str) -> Atom {
    if id.starts_with(AGENT_PREFIX) {
        Atom(id.to_owned())
    } else {
        Atom(format!("{AGENT_PREFIX}{id}"))
    }
}

/// A task kept in the workspace's room before the tracker, as the import
/// reads it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Imported {
    /// The workspace task's id: the issue's id derives from it.
    pub task: String,
    pub title: String,
    /// The instruction the task was given, as the issue's description.
    #[serde(default)]
    pub instruction: String,
    pub status: Status,
    /// `agent:<id>` for an agent's task.
    #[serde(default)]
    pub assignee: Option<String>,
    /// Who created it.
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub created_ms: i64,
    /// Its conversation, oldest first.
    #[serde(default)]
    pub messages: Vec<ImportedMessage>,
}

/// One turn of an imported task's conversation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImportedMessage {
    pub author: String,
    #[serde(default)]
    pub by_agent: bool,
    #[serde(default)]
    pub at_ms: i64,
    pub text: String,
}

impl Imported {
    /// The id the imported issue is kept under: the same on every device
    /// that imports the task, so two imports are one issue.
    #[must_use]
    pub fn issue_id(&self) -> Id {
        Id::from_raw(format!("workspace-task-{}", self.task))
    }

    /// The issue it becomes, numbered `number`.
    #[must_use]
    pub fn issue(&self, number: u64) -> Issue {
        let mut comments = Collection::new();
        for (index, message) in self.messages.iter().enumerate() {
            comments.push(
                Id::from_raw(format!("{}-{index}", self.issue_id())),
                Message {
                    author: message.author.clone(),
                    author_name: String::new(),
                    by_agent: message.by_agent,
                    at_ms: message.at_ms,
                    text: message.text.clone(),
                    kind: if message.by_agent {
                        MessageKind::FromRun
                    } else {
                        MessageKind::Comment
                    },
                    run: 0,
                    lookup: String::new(),
                    step: String::new(),
                    incorporated: true,
                    origin: None,
                },
            );
        }
        let title: String = self.title.trim().chars().take(MAX_TITLE_CHARS).collect();
        Issue {
            number,
            title: if title.is_empty() {
                "Task".to_owned()
            } else {
                title
            },
            body: RichText::from_markdown(self.instruction.trim()),
            status: self.status,
            assignee: self
                .assignee
                .as_deref()
                .filter(|assignee| assignee_parts(assignee).is_some())
                .map(|assignee| Atom(assignee.to_owned())),
            author: self.author.clone(),
            created_ms: self.created_ms,
            comments,
            ..Issue::default()
        }
    }
}
