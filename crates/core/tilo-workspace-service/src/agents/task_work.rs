//! What a device does around an agent's session on a task of Tasks that is worked on over many
//! sessions.
//!
//! A task given to an agent may take hours or weeks: sessions with waits between them, on
//! devices that restart. Everything a later session needs is kept with the task (its `work`) and
//! in the workspace's own Notes and Files, never beside them, so this module keeps nothing: it
//! is handed the task as the change that triggered the turn left it, and writes to it through
//! Tasks' own actions as the agent whose session it is, for the person the agent works for.
//! Tasks decides what those actions allow: a session over the task's budget, or after its
//! sessions were lost too often in a row, is refused by `session_began` itself.
//!
//! - Before a session it says the session began, makes the task's place when it has none (a
//!   note in Notes and a folder named for the task, kept on the task by `set_place`), and tells
//!   the session where they are. The folder is made in the workspace's files by Files' own
//!   `new-folder`, as the agent on its turn: an agent whose rules refuse it Files has none, and
//!   its session is told so.
//! - After a session it counts what the session cost and how long it worked (`session_ended`).
//! - A session that could not begin or finish (the task's budget, the day's limit, a failure)
//!   is written on the task as stopped, with its plain line (`stop`). Nothing here closes it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{
    APPLICATION_RESOURCE_PREFIX, Caller, InvokeArgument, RemoteActionTarget, workspace_provider,
};
use tilo_automation::dispatch::DocumentEvent;
use tilo_peer_state::document_frame::DocumentRoute;

use super::triggered::{self, TASKS};
use super::{Outcome, invoke_named, money};
use crate::workspaces::Workspaces;

/// The kind of what a task of [`TASKS`] keeps while an agent works it.
const WORK: &str = "tl.so.tilo.tasks.Work";
/// The app that keeps the workspace's notes.
const NOTES: &str = "so.tilo.notes";
/// The app that keeps the workspace's files: its one folder, the tree its members mount.
const FILES: &str = "so.tilo.file-explorer";
/// The longest name the folder of a task takes from its title, in characters.
const MOST_FOLDER_TITLE_CHARS: usize = 80;
/// How many sessions of a task may be lost in a row before Tasks begins no other.
const MOST_LOST_SESSIONS: u64 = 2;

/// What a new note of a task starts with: the three things a later session looks for.
const NOTE_STARTS: &str =
    "## Plan\n\n## Found so far\n\nEach finding with where it comes from.\n\n## Still open\n";

/// How a session on a task works and ends, as every one is told.
const HOW_TO_WORK: &str = "\n\nDo the work through the workspace's apps. Before you end, write \
    what a later session needs into the note. If you have to wait (for a reply, a time, a \
    resource to change, a person), call Tasks' `wait` on this task with what you wait for and \
    `until`, the latest moment the wait ends, say in one line what you wait for, and end: you \
    are started again when it comes or at that moment. Where one of your person's automations \
    can see what you wait for (a time, a resource of the workspace changing), set it up first \
    and name its id in the wait as `on`: when it comes, the task goes on with what the \
    automation says and nothing else is asked. Remove an automation you set up only for a wait \
    once the wait is over. Where the work falls into pieces that can be done apart, make each a \
    subtask of this one with `add_subtask` and give it to yourself with `assign`, say with \
    `set_pieces_at_once` how many are in work at once, give each a budget with `set_budget`, \
    and `wait` on this task with a wait of the kind `pieces`: you are started again when they \
    are through. When the work is finished, set the task's status to In review, never to Done: \
    that is its person's to say. If you cannot finish, say plainly why.";

/// What a task keeps of its work, as a session needs it: where its material is, what it may
/// cost and how long it may work, and how its sessions went.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Work {
    pub(super) folder: String,
    pub(super) note: String,
    pub(super) budget_micros: Option<u64>,
    pub(super) budget_ms: Option<u64>,
    pub(super) spent_micros: u64,
    pub(super) worked_ms: u64,
    pub(super) sessions: u64,
    /// Whether a session began and was never said to have ended.
    pub(super) interrupted: bool,
    pub(super) lost: u64,
}

impl Work {
    /// What is left of the task's money, where it has a budget.
    pub(super) fn money_left(&self) -> Option<u64> {
        self.budget_micros
            .map(|budget| budget.saturating_sub(self.spent_micros))
    }

    /// Why Tasks would refuse a session on a task that keeps this work, as its `stop` names it:
    /// what `session_began` holds a session to.
    fn stopped_why(&self) -> &'static str {
        if self
            .budget_ms
            .is_some_and(|budget| self.worked_ms >= budget)
        {
            "time"
        } else if self
            .budget_micros
            .is_some_and(|budget| self.spent_micros >= budget)
        {
            "budget"
        } else if self.interrupted && self.lost >= MOST_LOST_SESSIONS {
            "lost_worker"
        } else {
            "failed"
        }
    }

    /// The work the resource `held` of [`WORK`] keeps, as the store's index holds it.
    fn of(held: &tilo_resource_store::Indexed) -> Self {
        let number = |field: &str| match held.scalar(field) {
            tilo_resource_store::Value::Int(number) => u64::try_from(number).ok(),
            _ => None,
        };
        let text = |field: &str| {
            held.scalar(field)
                .as_text()
                .map(str::to_owned)
                .unwrap_or_default()
        };
        Self {
            folder: text("folder"),
            note: text("note"),
            budget_micros: number("budget_micros"),
            budget_ms: number("budget_ms"),
            spent_micros: number("spent_micros").unwrap_or_default(),
            worked_ms: number("worked_ms").unwrap_or_default(),
            sessions: number("sessions").unwrap_or_default(),
            interrupted: !matches!(
                held.scalar("session_since"),
                tilo_resource_store::Value::Null
            ),
            lost: number("lost").unwrap_or_default(),
        }
    }

    /// The work a task keeps in its `work` field: a record held there, or a resource the field
    /// refers to (its id, or a reference as tilolang writes one). The default where the task
    /// keeps none.
    async fn where_kept(workspaces: &Workspaces, workspace: &str, held: Option<&Value>) -> Self {
        // A value the record holds whole comes as its JSON form: `{"json": …}`.
        let held = held.cloned().map(|held| match held {
            Value::Object(mut wrapped) if wrapped.len() == 1 && wrapped.contains_key("json") => {
                wrapped.remove("json").unwrap_or_default()
            }
            other => other,
        });
        match unwrapped(held) {
            Some(Value::String(id)) if !id.is_empty() => Self::at(workspaces, workspace, &id).await,
            Some(Value::Object(record)) => match record.get("ref").and_then(Value::as_str) {
                Some(id) => Self::at(workspaces, workspace, id).await,
                None => match record.get("fields").and_then(Value::as_object) {
                    Some(fields) => Self::held(fields),
                    None => Self::held(&record),
                },
            },
            _ => Self::default(),
        }
    }

    /// The work the resource `id` of [`WORK`] keeps.
    async fn at(workspaces: &Workspaces, workspace: &str, id: &str) -> Self {
        workspaces
            .store_held(workspace, WORK, id)
            .await
            .map(|held| Self::of(&held))
            .unwrap_or_default()
    }

    /// The work a record held in its holder keeps, by the field names Tasks gives them: a field
    /// kept under its declaration's id is read by its name.
    fn held(fields: &Map<String, Value>) -> Self {
        let names = crate::tl_records::built_in(TASKS)
            .expect("the packages built into this build compile")
            .map(|packages| packages.field_names("Work"))
            .unwrap_or_default();
        let fields: Map<String, Value> = fields
            .iter()
            .map(|(key, value)| (names.get(key).unwrap_or(key).clone(), value.clone()))
            .collect();
        let fields = &fields;
        let number = |field: &str| {
            fields
                .get(field)
                .and_then(|held| unwrapped(Some(held.clone())))
                .and_then(|held| held.as_u64())
        };
        let text = |field: &str| {
            fields
                .get(field)
                .and_then(|held| unwrapped(Some(held.clone())))
                .and_then(|held| held.as_str().map(str::to_owned))
                .unwrap_or_default()
        };
        Self {
            folder: text("folder"),
            note: text("note"),
            budget_micros: number("budget_micros"),
            budget_ms: number("budget_ms"),
            spent_micros: number("spent_micros").unwrap_or_default(),
            worked_ms: number("worked_ms").unwrap_or_default(),
            sessions: number("sessions").unwrap_or_default(),
            interrupted: fields
                .get("session_since")
                .and_then(|held| unwrapped(Some(held.clone())))
                .is_some(),
            lost: number("lost").unwrap_or_default(),
        }
    }
}

/// A field's value as tilolang writes one: the value inside the tag a json field carries
/// (`json`), none for `none`, the value inside `some`, else the value itself.
fn unwrapped(held: Option<Value>) -> Option<Value> {
    match held? {
        Value::Null => None,
        Value::Object(tagged) if tagged.len() == 1 && tagged.contains_key("none") => None,
        Value::Object(mut tagged) if tagged.len() == 1 && tagged.contains_key("some") => {
            tagged.remove("some").and_then(|some| unwrapped(Some(some)))
        }
        Value::Object(mut tagged) if tagged.len() == 1 && tagged.contains_key("json") => {
            tagged.remove("json").and_then(|json| unwrapped(Some(json)))
        }
        other => Some(other),
    }
}

/// A task an agent's turn is about, as the change that triggered the turn left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// Its id in the workspace's store.
    pub(super) id: String,
    pub(super) number: u64,
    pub(super) title: String,
    pub(super) work: Work,
}

impl Task {
    /// The task `event` changed, as the change left it, with the work it keeps read where the
    /// task names it; `None` for a change of no task.
    pub(super) async fn of(
        workspaces: &Workspaces,
        workspace: &str,
        event: &DocumentEvent,
    ) -> Option<Self> {
        if event.kind != triggered::TASK {
            return None;
        }
        let id = event.item.as_ref()?.id.clone();
        let after = event.after.as_ref()?;
        let work = Work::where_kept(workspaces, workspace, after.get("work")).await;
        Some(Self {
            id,
            number: after
                .get("number")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            title: after
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            work,
        })
    }

    /// Its address in the workspace: `tl:so.tilo.tasks:<id>`.
    pub(super) fn address(&self) -> String {
        triggered::published(TASKS, &self.id)
    }
}

/// One task's work, as a session of its agent reaches it.
pub(super) struct TaskWork {
    pub(super) service: super::WorkspaceActionProvider,
    /// The agent the task is assigned to, on this turn of the person it works for.
    pub(super) caller: Caller,
    pub(super) workspace: String,
    pub(super) task: Task,
}

/// Where a task's working material is, and what its note reads now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) folder: String,
    /// Why the folder could not be made, when it could not: it is named and is not there.
    pub(super) no_folder: Option<String>,
    /// The note's resource in Notes; empty when none could be made.
    pub(super) note: String,
    /// Why no note could be made, when none could.
    pub(super) no_note: Option<String>,
}

/// The line of a task whose budget in money is spent.
fn spent_line(work: &Work) -> String {
    format!(
        "I stopped: this task's budget of {} is spent. Raise it to let me go on.",
        money(work.budget_micros.unwrap_or_default())
    )
}

/// The folder a task's material is kept in when nobody named one: `Tasks/<number> <title>`,
/// the title without what a folder may not be called.
fn folder_of(task: &Task) -> String {
    let title: String = task
        .title
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .take(MOST_FOLDER_TITLE_CHARS)
        .collect();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = title.trim_matches('.');
    if title.is_empty() {
        format!("Tasks/{}", task.number)
    } else {
        format!("Tasks/{} {title}", task.number)
    }
}

impl TaskWork {
    fn route(&self, app: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        }
    }

    /// Invoke Tasks' action `action` on the task, as the agent.
    async fn on_task(&self, action: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        invoke_named(
            &self.service,
            &self.caller,
            &self.route(TASKS),
            &self.task.address(),
            action,
            arguments,
        )
        .await
    }

    /// Begin a session on the task, as Tasks allows it: counted, and the task in progress.
    ///
    /// # Errors
    /// Tasks refused it (the task's budget or working time is spent, or its sessions were lost
    /// too often in a row), or did not take it: the plain line, which is written on the task as
    /// its stop.
    pub(super) async fn begin(&self) -> Result<(), String> {
        if let Err(refused) = self.on_task("session_began", Map::new()).await {
            self.stopped(self.task.work.stopped_why(), &refused).await;
            return Err(refused);
        }
        Ok(())
    }

    /// Count the session that ended as `outcome`: what it cost and how long it worked. One that
    /// could not finish is written on the task as stopped, and one that ran out of the task's
    /// own money says that rather than the day's limit: the outcome its answer says.
    pub(super) async fn ended_as(
        &self,
        outcome: Outcome,
        cost_micros: u64,
        worked: Duration,
    ) -> Outcome {
        let before = &self.task.work;
        let mut counted = Map::new();
        counted.insert("spent_micros".into(), json!(cost_micros));
        counted.insert(
            "worked_ms".into(),
            json!(u64::try_from(worked.as_millis()).unwrap_or(u64::MAX)),
        );
        if let Err(error) = self.on_task("session_ended", counted).await {
            tracing::warn!(workspace = %self.workspace, task = %self.task.id, %error, "a session on a task is not counted");
        }
        match outcome {
            Outcome::Answered(words) => Outcome::Answered(words),
            Outcome::Stopped(_) if before.money_left().is_some_and(|left| cost_micros >= left) => {
                let words = spent_line(before);
                self.stopped("budget", &words).await;
                Outcome::Stopped(words)
            }
            Outcome::Stopped(words) => {
                self.stopped("failed", &words).await;
                Outcome::Stopped(words)
            }
            Outcome::Failed(words) => {
                self.stopped("failed", &words).await;
                Outcome::Failed(words)
            }
            Outcome::Cancelled => {
                self.stopped("person", "Its person stopped the work.").await;
                Outcome::Cancelled
            }
        }
    }

    /// Write on the task that its work stopped, `why` as Tasks' `StoppedWhy` names it, in
    /// `words`.
    async fn stopped(&self, why: &str, words: &str) {
        let mut arguments = Map::new();
        arguments.insert("why".into(), json!(why));
        arguments.insert("words".into(), json!(words));
        if let Err(error) = self.on_task("stop", arguments).await {
            tracing::warn!(workspace = %self.workspace, task = %self.task.id, %error, "a task's stop is not written");
        }
    }

    /// The task's place, made when it has none: its folder's path and its note, kept on the
    /// task. A note that cannot be made leaves the place with its folder alone, and the turn is
    /// told so. The folder is made in the workspace's files before every session
    /// ([`Self::make_folder`]); one that cannot be made keeps its name, and the turn is told it
    /// is not there.
    pub(super) async fn place(&self) -> Place {
        let kept = &self.task.work;
        let folder = if kept.folder.is_empty() {
            folder_of(&self.task)
        } else {
            kept.folder.clone()
        };
        let mut note = kept.note.clone();
        let mut no_note = None;
        if note.is_empty() {
            match self.make_note().await {
                Ok(made) => note = made,
                Err(error) => {
                    tracing::warn!(workspace = %self.workspace, task = %self.task.id, %error, "a task's note is not made");
                    no_note = Some(error);
                }
            }
        }
        if kept.folder != folder || kept.note != note {
            let mut arguments = Map::new();
            arguments.insert("folder".into(), json!(folder));
            arguments.insert("note".into(), json!(note));
            if let Err(error) = self.on_task("set_place", arguments).await {
                tracing::warn!(workspace = %self.workspace, task = %self.task.id, %error, "a task's place is not kept");
            }
        }
        let no_folder = self.make_folder(&folder).await.err();
        if let Some(error) = &no_folder {
            tracing::warn!(workspace = %self.workspace, task = %self.task.id, %error, "a task's folder is not made");
        }
        Place {
            folder,
            no_folder,
            note,
            no_note,
        }
    }

    /// Make the folder `folder` in the workspace's files, with the folders on the way to it:
    /// Files' own `new-folder` by a path in the workspace, asked as the agent on this turn, so
    /// with what its rules allow it of Files. A folder that is there is left as it is: every
    /// session asks, none makes a second one, and one its person took away is there again.
    ///
    /// # Errors
    /// The agent's rules refuse it Files, the path is none of the workspace, or the workspace
    /// did not take the change: why the folder is not there, in words the turn is told.
    async fn make_folder(&self, folder: &str) -> Result<(), String> {
        let (parent, name) = folder.rsplit_once('/').unwrap_or(("", folder));
        let mut arguments = Map::new();
        arguments.insert("parent".into(), json!(parent));
        arguments.insert("name".into(), json!(name));
        let target = RemoteActionTarget {
            provider: workspace_provider(&self.workspace),
            address: ActionAddress {
                resource: format!("{APPLICATION_RESOURCE_PREFIX}{FILES}"),
                action: "new-folder".to_owned(),
            },
        };
        let asked = InvokeArgument::Json(arguments);
        super::invoke_at(Arc::new(self.service.clone()), &self.caller, target, asked)
            .await
            .map(drop)
    }

    /// Make the task's note in Notes, titled after it: the note's resource, which `set_place`
    /// keeps on the task.
    async fn make_note(&self) -> Result<String, String> {
        // The agent opens Notes in the workspace, as it does with its own tools: an app's
        // actions are offered to a session once it opened the app.
        let opened = super::invoke_at(
            Arc::new(self.service.clone()),
            &self.caller,
            tilo_actions::remote::application_open_target(&self.workspace, NOTES),
            InvokeArgument::None,
        )
        .await;
        if let Err(error) = opened {
            tracing::debug!(workspace = %self.workspace, %error, "Notes did not open for a task's note");
        }
        let mut arguments = Map::new();
        arguments.insert(
            "title".into(),
            json!(format!("#{} {}", self.task.number, self.task.title)),
        );
        arguments.insert("body".into(), json!(NOTE_STARTS));
        let made = invoke_named(
            &self.service,
            &self.caller,
            &self.route(NOTES),
            &format!("tl:{NOTES}"),
            "new_note",
            arguments,
        )
        .await?;
        made.pointer("/resources/0")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("Notes made no note: {made}"))
    }
}

/// What an agent is told of the task `event` changed: the task, what it says, and why it is
/// asked now when it was told to go on.
pub(super) fn asked(event: &DocumentEvent, task: &Task) -> String {
    let mut request = format!(
        "Task #{} \"{}\" in Tasks (`{}`) is assigned to you.",
        task.number,
        task.title,
        task.address()
    );
    let notes = triggered::said_in(event, &["notes"]);
    if !notes.is_empty() {
        request.push_str("\n\nWhat it says:\n");
        request.push_str(&notes);
    }
    let resumed = matches!(&event.did, tilo_actions::entry::Did::Verb(verb) if verb.action.action == "resume");
    if resumed {
        let cause = event
            .arguments
            .get("cause")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if cause.is_empty() {
            request.push_str(
                "\n\nYou let it wait, and what you waited for did not come by the moment you \
                 set. The wait is over: decide how to go on.",
            );
        } else {
            request.push_str("\n\nYou are told to go on with it: ");
            request.push_str(cause);
            request.push_str(
                ". Where it waited for its pieces, read what each left in its comments and its \
                 note before you go on.",
            );
        }
    }
    request
}

/// `request` with what a session needs beside the ask: that earlier ones happened (`work` is what
/// the task kept before this one), where its working material is, what is left of its budget,
/// and how to wait and how to end.
pub(super) fn with_place(request: &str, work: &Work, place: &Place) -> String {
    let mut request = request.to_owned();
    if work.sessions > 0 {
        request.push_str(
            "\n\nThis is not your first session on it, and you remember nothing of the earlier \
             ones: everything you know of them is in its note, its folder and its comments.",
        );
    }
    if work.interrupted {
        request.push_str(
            " The session before this one was interrupted before it ended: look at what it \
             already did before you do anything a second time.",
        );
    }
    request.push_str(
        "\n\nIts working material is kept in the workspace, and a later session starts from it \
         alone:",
    );
    if place.note.is_empty() {
        let why = place.no_note.as_deref().unwrap_or("it gave no reason");
        let none = format!(
            "\n- It has no note: Notes could not make one ({why}). Keep what a later session \
             needs in the task's comments."
        );
        request.push_str(&none);
    } else {
        let note = format!(
            "\n- Its note in Notes (`{}`): read it first. Keep the plan, what you found, each \
             finding with where it comes from, and what is still open there. Add to it or \
             rewrite it as you go.",
            place.note
        );
        request.push_str(&note);
    }
    let folder = match place.no_folder.as_deref() {
        None => format!(
            "\n- The folder `{}` in the workspace's files: put files there.",
            place.folder
        ),
        Some(why) => format!(
            "\n- It has no folder: `{}` could not be made in the workspace's files ({why}). \
             Put no files there; keep what a later session needs in its note or its comments.",
            place.folder
        ),
    };
    request.push_str(&folder);
    if let Some(left) = work.money_left() {
        let budget = format!(
            "\n\nIts budget: {} of {} is left for all the work on it.",
            money(left),
            money(work.budget_micros.unwrap_or_default())
        );
        request.push_str(&budget);
    }
    request.push_str(HOW_TO_WORK);
    request
}

/// A task of the workspace as the store's index holds it, for what the engine reads of tasks
/// beside a turn: the look round, the proposals filed as tasks, the waits an automation ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Held {
    pub(super) id: String,
    pub(super) number: u64,
    pub(super) title: String,
    /// Its status, by its case's name: `backlog`, `todo`, `in_progress`, `in_review`, `done`,
    /// `cancelled`.
    pub(super) status: String,
    /// Who is doing it, as the actor's JSON text; empty for nobody.
    pub(super) assignee: String,
    /// Who gave it to the agent it is assigned to, as the actor's JSON text; empty for nobody.
    pub(super) assigned_by: String,
    /// The day it is due by, `YYYY-MM-DD`, and the moment on that day.
    pub(super) due: Option<String>,
    pub(super) at: Option<String>,
}

impl Held {
    fn of(id: String, held: &tilo_resource_store::Indexed) -> Self {
        let text = |field: &str| held.scalar(field).as_text().map(str::to_owned);
        Self {
            number: match held.scalar("number") {
                tilo_resource_store::Value::Int(number) => u64::try_from(number).unwrap_or(0),
                _ => 0,
            },
            title: text("title").unwrap_or_default(),
            status: text("status").unwrap_or_default(),
            assignee: text("assignee").unwrap_or_default(),
            assigned_by: text("assigned_by").unwrap_or_default(),
            due: text("due").filter(|due| !due.is_empty()),
            at: text("at").filter(|at| !at.is_empty()),
            id,
        }
    }

    /// Its address in the workspace: `tl:so.tilo.tasks:<id>`.
    pub(super) fn address(&self) -> String {
        triggered::published(TASKS, &self.id)
    }

    /// Whether it is closed: done or cancelled.
    pub(super) fn closed(&self) -> bool {
        matches!(self.status.as_str(), "done" | "cancelled")
    }

    /// Whether `member` is doing it.
    pub(super) fn assigned_to(&self, member: &tilo_action_policy::MemberId) -> bool {
        !self.assignee.is_empty() && self.assignee.contains(&triggered::names(member))
    }

    /// The agent doing it, by its id, when an agent is.
    pub(super) fn agent(&self) -> Option<String> {
        /// The id under `agent` anywhere in `value`.
        fn agent_in(value: &Value) -> Option<String> {
            match value {
                Value::Object(fields) => fields
                    .get("agent")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| fields.values().find_map(agent_in)),
                _ => None,
            }
        }
        agent_in(&serde_json::from_str(&self.assignee).ok()?)
    }

    /// Whether a person gave it to the agent doing it.
    pub(super) fn given_by_a_person(&self) -> bool {
        self.assigned_by.contains("\"person\"")
    }

    /// When it is due, in milliseconds since the epoch: its moment, or the start of its day.
    pub(super) fn due_ms(&self) -> Option<i64> {
        if let Some(at) = self.at.as_deref().and_then(|at| {
            tilo_failure::ignored!(
                "a due moment that does not parse names none",
                at.parse::<jiff::Timestamp>().ok()
            )
        }) {
            return Some(at.as_millisecond());
        }
        let day = tilo_failure::ignored!(
            "a due day that does not parse names none",
            self.due.as_deref()?.parse::<jiff::civil::Date>().ok()?
        );
        day.to_zoned(jiff::tz::TimeZone::UTC)
            .ok()
            .map(|start| start.timestamp().as_millisecond())
    }
}

/// Every task of `workspace` this device holds that holds to `filters`.
///
/// # Errors
/// The workspace is not kept here.
pub(super) async fn tasks(
    workspaces: &Workspaces,
    workspace: &str,
    filters: Vec<tilo_resource_store::Filter>,
) -> Result<Vec<Held>, String> {
    let mut query = tilo_resource_store::Query::of(triggered::TASK, 500);
    query.filters = filters;
    Ok(workspaces
        .store_found(workspace, query)
        .await?
        .into_iter()
        .map(|(id, held)| Held::of(id, &held))
        .collect())
}

/// The id of the task at `address` (`tl:so.tilo.tasks:<id>`), when it names one.
pub(super) fn task_id(address: &str) -> Option<&str> {
    address
        .strip_prefix("tl:")?
        .strip_prefix(TASKS)?
        .strip_prefix(':')
        .filter(|id| !id.is_empty())
}

/// The task `id` of `workspace`, as this device holds it.
pub(super) async fn task(workspaces: &Workspaces, workspace: &str, id: &str) -> Option<Held> {
    workspaces
        .store_held(workspace, triggered::TASK, id)
        .await
        .map(|held| Held::of(id.to_owned(), &held))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(title: &str) -> Task {
        Task {
            id: "doc/one".to_owned(),
            number: 12,
            title: title.to_owned(),
            work: Work::default(),
        }
    }

    #[test]
    fn a_tasks_folder_is_named_by_its_number_and_a_title_a_folder_may_take() {
        assert_eq!(
            folder_of(&task("Book the venue / the date.")),
            "Tasks/12 Book the venue the date"
        );
        assert_eq!(folder_of(&task(" ... ")), "Tasks/12");
    }
}

#[cfg(all(test, feature = "apps"))]
mod session_tests {
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
        InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, ResourceReads,
    };
    use tilo_actions::{ActionInvoker, ActionOutput};
    use tilo_resource_store::wire::{Command, Reply};
    use tilo_server_protocol::WorkspaceId;

    use super::*;
    use crate::workspaces::fixtures::{TestGroupKeys, TestHost, governor, test_host_holding};

    /// The packages the device runs, each with its ids lock: Tasks and what it builds on.
    const PACKAGES: [(&str, &str); 4] = [
        (
            include_str!("../../../../../apps/tl/discussion/app.tl"),
            include_str!("../../../../../apps/tl/discussion/app.ids"),
        ),
        (
            include_str!("../../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../../apps/tl/notes/app.ids"),
        ),
        (
            concat!(
                include_str!("../../../../../apps/tl/contacts/app.tl"),
                "\n",
                include_str!("../../../../../apps/tl/contacts/identities.tl")
            ),
            include_str!("../../../../../apps/tl/contacts/app.ids"),
        ),
        (
            include_str!("../../../../../apps/tl/tasks/app.tl"),
            include_str!("../../../../../apps/tl/tasks/app.ids"),
        ),
    ];

    /// The documents a device's store changed, each whole: what sync carries to the workspace.
    type Documents = Vec<(String, Vec<u8>)>;

    /// One invocation of a package's action on the device, and where its answer goes.
    struct Ask {
        invoker: ActionInvoker,
        resource: String,
        action: String,
        arguments: Map<String, Value>,
        answer: mpsc::Sender<(Result<ActionOutput, String>, Documents)>,
    }

    /// The person's device: tilolang's host over a store of its own, on a thread of its own (a
    /// host is held by one thread), invoked as the device's action service invokes it.
    fn device(person: String, asks: mpsc::Receiver<Ask>, opened: mpsc::Sender<Documents>) {
        use tilo_service::Service as _;
        use tilolang_tilo::resources::{LocalPort, Port, ResourceStore};

        let port = Rc::new(LocalPort::in_memory());
        let store = ResourceStore::new(Rc::clone(&port) as Rc<dyn Port>);
        let viewer = tilo_actions::Actor::Member(tilo_actions::MemberId::person(person.clone()));
        let host = tilolang_tilo::host::Host::open(store, viewer, None, || None).unwrap();
        for (source, ids) in PACKAGES {
            host.load_builtin(source, ids).unwrap();
        }
        // The roots of the device's apps (its Tasks, which a new task is added to, its
        // Notebook, which a task's note is made in): made as an app makes its root, by the
        // document each is kept in.
        let roots: BTreeMap<String, String> = [TASKS, "so.tilo.notes"]
            .into_iter()
            .map(|app| (format!("tl:{app}"), host.root_of(app).unwrap().doc))
            .collect();
        let mut service = tilolang_service::ResourceService::new(host, person);
        let mut sent: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut moved = || {
            let mut documents = Vec::new();
            for doc in port.documents() {
                let Some(bytes) = port.save(&doc) else {
                    continue;
                };
                if sent.get(&doc) != Some(&bytes) {
                    sent.insert(doc.clone(), bytes.clone());
                    documents.push((doc, bytes));
                }
            }
            documents
        };
        opened.send(moved()).unwrap();
        while let Ok(ask) = asks.recv() {
            // An app's own actions are asked of the app (`tl:<app>`): its root, as the
            // device's service publishes it.
            let resource = match roots.get(&ask.resource) {
                Some(doc) => service
                    .resources()
                    .into_iter()
                    .find(|resource| resource.id.as_str().contains(doc.as_str()))
                    .unwrap_or_else(|| panic!("the device publishes {}'s root", ask.resource))
                    .id
                    .as_str()
                    .to_owned(),
                None => ask.resource.clone(),
            };
            let started = service.invoke_as(&ask.invoker, &resource, &ask.action, &ask.arguments);
            let done = match started {
                Ok(tilo_service::Started::Done(output)) => Ok(output),
                Ok(tilo_service::Started::Accepted) => Ok(ActionOutput::default()),
                Ok(tilo_service::Started::Later(later)) => later
                    .wait_timeout(std::time::Duration::from_secs(10))
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| panic!("{} did not end", ask.action))
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = ask.answer.send((done, moved()));
        }
    }

    /// The device's broker as a turn's calls reach it: it decides each call (`refuses` names
    /// the actions it denies the turn, with its words) and keeps who asked what. A call it lets
    /// through to a package runs there, Tasks' own rules deciding it; any other (Files') is
    /// answered as done.
    struct Broker {
        workspaces: Workspaces,
        workspace: String,
        refuses: BTreeMap<&'static str, &'static str>,
        device: Mutex<mpsc::Sender<Ask>>,
        asked: Mutex<Vec<(Caller, String, String, Map<String, Value>)>>,
    }

    /// Run `action` on `resource` on the device as `invoker`, and carry what it wrote to the
    /// workspace's store: what the action answered, or the words it was refused with.
    fn on_device(
        device: &Mutex<mpsc::Sender<Ask>>,
        workspaces: &Workspaces,
        workspace: &str,
        invoker: ActionInvoker,
        resource: &str,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<ActionOutput, String> {
        let (answer, answered) = mpsc::channel();
        device
            .lock()
            .unwrap()
            .send(Ask {
                invoker,
                resource: resource.to_owned(),
                action: action.to_owned(),
                arguments,
                answer,
            })
            .unwrap();
        let (done, documents) = answered.recv().unwrap();
        tokio::runtime::Handle::current().block_on(workspaces.take_documents(workspace, documents));
        done
    }

    struct Provider(Arc<Broker>);

    impl ActionProvider for Provider {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            let broker = &self.0;
            let resource = request.target.address.resource.clone();
            let action = request.target.address.action.clone();
            let arguments = match &request.argument {
                InvokeArgument::Json(arguments) => arguments.clone(),
                _ => Map::new(),
            };
            broker.asked.lock().unwrap().push((
                caller.clone(),
                resource.clone(),
                action.clone(),
                arguments.clone(),
            ));
            if let Some(words) = broker.refuses.get(action.as_str()) {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: (*words).to_owned(),
                }));
            }
            let output = if resource.starts_with("tl:") {
                let ran = on_device(
                    &broker.device,
                    &broker.workspaces,
                    &broker.workspace,
                    ActionInvoker::admitted(caller.acting()),
                    &resource,
                    &action,
                    arguments,
                );
                match ran {
                    Ok(output) => output,
                    Err(reason) => {
                        return Pending::ready(Err(ActionCallError::Refused { reason }));
                    }
                }
            } else {
                ActionOutput::default()
            };
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed { output },
            }))
        }

        fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
            None
        }

        fn next_change(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<Pending<()>> {
            None
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }

        fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
            None
        }
    }

    struct Keys;

    impl super::super::agent_keys::AgentKeys for Keys {
        fn grant(&self, _: WorkspaceId, _: &str, _: &str, _: [u8; 32]) -> Result<[u8; 32], String> {
            Ok([7; 32])
        }

        fn sign(&self, _: WorkspaceId, _: &[u8; 32], _: &mut InvokeRequest) -> Result<(), String> {
            Ok(())
        }

        fn made(
            &self,
            _key: &[u8; 32],
        ) -> Option<(tilo_user_identity::DeviceCert, ed25519_dalek::SigningKey)> {
            None
        }
    }

    /// A workspace of a test host, its person, and Tasks on the person's device.
    struct Host {
        host: TestHost,
        _dir: tempfile::TempDir,
        workspaces: Workspaces,
        workspace: String,
        /// The person, by their root in hex.
        person: String,
        /// The person's root.
        root: [u8; 32],
        device: Mutex<mpsc::Sender<Ask>>,
    }

    impl Host {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (key, cert) = governor();
            let host =
                test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default()))
                    .await;
            let workspaces = host.workspaces().clone();
            workspaces.set_member_device(Arc::new(crate::workspaces::MemberDevice {
                cert: cert.clone(),
                signer: Arc::new(key.clone()),
            }));
            let workspace = workspaces.create("Tasks").await.unwrap().workspace;
            let route = tilo_peer_state::document_frame::DocumentRoute {
                app_id: TASKS.into(),
                cluster_id: workspace.clone(),
                document: String::new(),
            };
            let _ = workspaces.own_route_keys(&route, true).await.unwrap();
            let person = hex::encode(cert.user_public_key);
            let (device, asks) = mpsc::channel();
            let (opened, documents) = mpsc::channel();
            std::thread::Builder::new()
                .name("tasks-device".into())
                .spawn({
                    let person = person.clone();
                    move || self::device(person, asks, opened)
                })
                .unwrap();
            let documents = tokio::task::spawn_blocking(move || documents.recv().unwrap())
                .await
                .unwrap();
            workspaces.take_documents(&workspace, documents).await;
            Self {
                host,
                _dir: dir,
                workspaces,
                workspace,
                person,
                root: cert.user_public_key,
                device: Mutex::new(device),
            }
        }

        /// Run `action` on `resource` as the person, on their device: the resources it made.
        ///
        /// # Panics
        /// Tasks refused it.
        async fn act(&self, resource: &str, action: &str, arguments: Value) -> Vec<String> {
            let Value::Object(arguments) = arguments else {
                panic!("arguments by name: {arguments}")
            };
            let device = self.device.lock().unwrap().clone();
            let (workspaces, workspace) = (self.workspaces.clone(), self.workspace.clone());
            let person = self.person.clone();
            let (resource, action) = (resource.to_owned(), action.to_owned());
            let named = action.clone();
            tokio::task::spawn_blocking(move || {
                on_device(
                    &Mutex::new(device),
                    &workspaces,
                    &workspace,
                    ActionInvoker::person(person),
                    &resource,
                    &action,
                    arguments,
                )
            })
            .await
            .unwrap()
            .unwrap_or_else(|refused| panic!("{named}: {refused}"))
            .resources
        }

        /// A new task of Tasks titled `title`, with `more` of what a new task is given: its id.
        async fn new_task(&self, title: &str, more: Value) -> String {
            let mut arguments = json!({ "title": title });
            if let (Value::Object(all), Value::Object(more)) = (&mut arguments, more) {
                all.extend(more);
            }
            let made = self
                .act(&format!("tl:{TASKS}"), "new_task", arguments)
                .await;
            task_id(&made[0]).unwrap().to_owned()
        }

        /// The task `id` as the device holds it, its work read where Tasks keeps it.
        async fn task(&self, id: &str) -> Task {
            // Its number and title as the store's row holds them, its work as its content
            // keeps it.
            let held = super::task(&self.workspaces, &self.workspace, id)
                .await
                .unwrap_or_else(|| panic!("the task {id} is held"));
            let content = self.content(id).await;
            Task {
                id: id.to_owned(),
                number: held.number,
                title: held.title,
                work: Work::where_kept(&self.workspaces, &self.workspace, content.get("work"))
                    .await,
            }
        }

        /// The task `id` as the store keeps it.
        async fn content(&self, id: &str) -> Map<String, Value> {
            let command = Command::Content {
                kind: triggered::TASK.to_owned(),
                id: id.to_owned(),
            };
            let reply = self
                .workspaces
                .read_store_as(&self.workspace, &self.person, &command)
                .await
                .unwrap();
            let Reply::Content(Some(Value::Object(content))) = reply else {
                panic!("the task {id} is not kept: {reply:?}")
            };
            content
        }

        fn broker(&self, refuses: &[(&'static str, &'static str)]) -> Arc<Broker> {
            Arc::new(Broker {
                workspaces: self.workspaces.clone(),
                workspace: self.workspace.clone(),
                refuses: refuses.iter().copied().collect(),
                device: Mutex::new(self.device.lock().unwrap().clone()),
                asked: Mutex::default(),
            })
        }

        /// The engine's actions through `broker`.
        fn service(&self, broker: &Arc<Broker>) -> super::super::WorkspaceActionProvider {
            let provider: Arc<dyn ActionProvider + Send + Sync> =
                Arc::new(Provider(Arc::clone(broker)));
            super::super::WorkspaceActionProvider::new(
                super::super::EngineActions::new(provider, Arc::new(Keys)),
                WorkspaceId::parse(&self.workspace).unwrap(),
            )
        }

        /// The turn's agent, `scout`, acting for the person, as its calls name it.
        fn caller(&self) -> Caller {
            Caller::authenticated(self.root, [7; 32])
                .in_workspace(self.workspace.clone())
                .delegated_to("scout")
        }

        async fn end(self) {
            self.host.shutdown().await;
        }
    }

    /// A workspace whose Tasks holds the task "Book the venue", its work as its person's
    /// `before` actions on it left it, and a turn of the agent on it through a broker that
    /// denies `refuses`.
    async fn worked(
        before: &[(&str, Value)],
        refuses: &[(&'static str, &'static str)],
    ) -> (Host, TaskWork, Arc<Broker>) {
        let host = Host::new().await;
        let id = host
            .new_task("Book the venue", json!({"assignee": {"agent": "scout"}}))
            .await;
        for (action, arguments) in before {
            host.act(&triggered::published(TASKS, &id), action, arguments.clone())
                .await;
        }
        let broker = host.broker(refuses);
        let service = host.service(&broker);
        let task = host.task(&id).await;
        let work = TaskWork {
            service,
            caller: host.caller(),
            workspace: host.workspace.clone(),
            task,
        };
        (host, work, broker)
    }

    /// Which actions were asked, in order.
    fn actions(broker: &Broker) -> Vec<String> {
        broker
            .asked
            .lock()
            .unwrap()
            .iter()
            .map(|(_, _, action, _)| action.clone())
            .collect()
    }

    /// A session Tasks refuses (the task's budget is spent) does not begin: the turn ends with
    /// Tasks' words, which stop the task's work as spent, and no place is made for it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_tasks_refuses_ends_with_its_words_and_makes_nothing() {
        const SPENT: &str = "I stopped: this task's budget is spent. Raise it to let me go on.";
        let (host, work, broker) = worked(
            &[
                ("set_budget", json!({"micros": 10})),
                ("session_began", json!({})),
                ("session_ended", json!({"spent_micros": 10})),
            ],
            &[("session_began", SPENT)],
        )
        .await;
        assert_eq!(
            work.task.work.money_left(),
            Some(0),
            "the budget is spent: {:?}",
            host.content(&work.task.id).await.get("work")
        );
        let refused = work.begin().await.unwrap_err();
        assert!(refused.contains(SPENT), "{refused}");
        assert_eq!(actions(&broker), ["session_began", "stop"]);
        let stop = broker.asked.lock().unwrap()[1].3.clone();
        assert_eq!(stop["why"], "budget");
        assert!(stop["words"].as_str().unwrap().contains(SPENT));
        host.end().await;
    }

    /// A session that begins is told that sessions came before, what is left of its budget and
    /// where its material is, made for it through Tasks, Notes and Files as the agent; one that
    /// runs out of the task's money stops the task's work as spent, and what it cost is counted
    /// where Tasks keeps it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_is_told_its_tasks_work_and_counted_when_it_ends() {
        let (host, work, broker) = worked(
            &[
                ("set_budget", json!({"micros": 1_000_000})),
                ("session_began", json!({})),
                ("session_ended", json!({"spent_micros": 125_000})),
                ("session_began", json!({})),
                ("session_ended", json!({"spent_micros": 125_000})),
            ],
            &[],
        )
        .await;
        assert_eq!(work.task.work.sessions, 2);
        assert_eq!(work.task.work.money_left(), Some(750_000));
        work.begin().await.unwrap();
        let place = work.place().await;
        let request = with_place("Book the venue", &work.task.work, &place);
        assert!(request.contains("not your first session"), "{request}");
        assert!(request.contains("$0.75 of $1.00 is left"), "{request}");
        assert!(request.contains("`tl:so.tilo.notes:"), "{request}");
        assert!(
            request.contains("`Tasks/1 Book the venue`"),
            "{request}\nthe task as the store keeps it: {:?}",
            host.content(&work.task.id).await
        );
        assert_eq!(
            actions(&broker),
            [
                "session_began",
                "open",
                "new_note",
                "set_place",
                "new-folder"
            ]
        );
        let ended = work
            .ended_as(
                Outcome::Stopped("the model ran out".into()),
                750_000,
                Duration::from_secs(3),
            )
            .await;
        assert!(
            matches!(&ended, Outcome::Stopped(words) if words.contains("budget of $1.00 is spent")),
            "{ended:?}"
        );
        let asked = broker.asked.lock().unwrap().clone();
        let (_, counted, stop) = (&asked[5], &asked[5].3, &asked[6].3);
        assert_eq!(asked[5].2, "session_ended");
        assert_eq!(counted["spent_micros"], 750_000);
        assert_eq!(counted["worked_ms"], 3_000);
        assert_eq!(asked[6].2, "stop");
        assert_eq!(stop["why"], "budget");
        let after = host.task(&work.task.id).await;
        assert_eq!(after.work.spent_micros, 1_000_000);
        assert_eq!(after.work.money_left(), Some(0));
        assert!(!after.work.interrupted);
        // Every write is the agent's own, on the task it works or the place made for it.
        for (caller, asked_on, action, _) in &asked {
            assert_eq!(caller, &work.caller, "{action}");
            if !matches!(action.as_str(), "open" | "new_note" | "new-folder") {
                assert_eq!(asked_on, &work.task.address(), "{action}");
            }
        }
        host.end().await;
    }

    /// An agent the broker lets write nothing on the task begins no session on it, and nothing
    /// is asked as anyone else: the turn ends with the broker's refusal.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_without_a_grant_on_the_task_writes_nothing_to_it() {
        const DENIED: &str = "the agent may not change this task";
        let (host, work, broker) = worked(
            &[],
            &[
                ("session_began", DENIED),
                ("stop", DENIED),
                ("set_place", DENIED),
                ("new_note", DENIED),
            ],
        )
        .await;
        let refused = work.begin().await.unwrap_err();
        assert!(refused.contains(DENIED), "{refused}");
        assert_eq!(actions(&broker), ["session_began", "stop"]);
        assert!(
            broker
                .asked
                .lock()
                .unwrap()
                .iter()
                .all(|(caller, ..)| caller == &work.caller)
        );
        host.end().await;
    }
}
