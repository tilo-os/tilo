//! One issue and what it holds: its thread and its links.

use serde::{Deserialize, Serialize};
use tilo_document::RichText;
use tilo_resource::{self, Atom, Collection, Id, JsonSchema, Model, Refusal, Set};

use crate::model::goal::Area;

/// The longest title an issue keeps.
pub const MAX_TITLE_CHARS: usize = 240;
/// The longest message a thread keeps.
pub const MAX_MESSAGE_CHARS: usize = 16 * 1024;
/// The most labels one issue carries.
pub const MAX_LABELS: usize = 32;

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
    /// Whether an agent wrote it.
    #[model(readonly)]
    pub by_agent: bool,
    #[model(readonly)]
    pub at_ms: i64,
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
    /// Whom the issue is assigned to: `person:<id>` or `agent:<id>`
    /// ([`person_assignee`], [`agent_assignee`]). Written whole, so two
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
    /// How urgent it is, from 1 (the most urgent) to 4; None for one nobody
    /// ranked.
    pub priority: Option<u8>,
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
    #[model(readonly)]
    pub created_ms: i64,
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
            return Err(Refusal::new(
                "an assignee is a person or an agent: person:<id> or agent:<id>",
            ));
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
}

const PERSON_PREFIX: &str = "person:";
const AGENT_PREFIX: &str = "agent:";

fn assignee_parts(assignee: &str) -> Option<(&'static str, &str)> {
    let (prefix, id) = if let Some(id) = assignee.strip_prefix(PERSON_PREFIX) {
        (PERSON_PREFIX, id)
    } else {
        (AGENT_PREFIX, assignee.strip_prefix(AGENT_PREFIX)?)
    };
    (!id.trim().is_empty() && id.trim() == id).then_some((prefix, id))
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
