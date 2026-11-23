//! What a workspace's server does for a task that is worked on over many agent sessions.
//!
//! A task given to an agent may take hours or weeks: sessions with waits between them, on a
//! server that restarts. Everything a later session needs is kept with the task (Tasks' `Work`)
//! and in the workspace's own Notes and Files, never beside them, so this module keeps nothing:
//! it reads the task through the store's typed reads as the turn's agent for the person it acts
//! for, and writes to it through Tasks' own actions under the agent's own key.
//!
//! - Before a session it says the session began. Tasks decides whether one may (its budget in
//!   money and in time, sessions lost too often in a row) and refuses in the words its person
//!   reads: the turn then ends with those words, said on the task, and the task's work is
//!   written as stopped. Otherwise it makes the task's place when it has none (a note in Notes
//!   made from the task, and a folder named for it) and tells the session what it needs: what
//!   made the task go on, what earlier sessions left, and what is left of its budget. The folder
//!   is made in the workspace's files by Files' own `new-folder`, as the agent on its turn: an
//!   agent whose rules refuse it Files has none, and its session is told so.
//! - After a session it counts what the session cost and how long it worked.
//! - A session that could not finish (the task's budget, the day's limit, a failure) is written
//!   on the task as stopped, with its plain line. Nothing here writes `done`.
//!
//! The same is done for an issue of the tracker's own documents ([`IssueWork`]), which only an
//! issue's own turn reaches.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tasks_model::{Asked, Issue, IssueAsk, MOST_LOST_SESSIONS, StoppedWhy, Work};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{
    APPLICATION_RESOURCE_PREFIX, Caller, InvokeArgument, RemoteActionTarget, ResourceReads,
    workspace_provider,
};
use tilo_peer_state::document_frame::DocumentRoute;

use super::tl_tasks::{self, Reader};
use super::{Outcome, TASKS, invoke_named, money};
use crate::workspaces::Workspaces;

/// The app that keeps the workspace's notes.
const NOTES: &str = "so.tilo.notes";
/// The app that keeps the workspace's files: its one folder, the tree its members mount.
const FILES: &str = "so.tilo.file-explorer";
/// The longest name the folder of an issue takes from its title, in characters.
const MOST_FOLDER_TITLE_CHARS: usize = 80;

/// What a new note of an issue starts with: the three things a later session looks for.
const NOTE_STARTS: &str =
    "## Plan\n\n## Found so far\n\nEach finding with where it comes from.\n\n## Still open\n";

/// How a session on a task works and ends, as every one is told, by the names Tasks gives its
/// actions.
const HOW_TO_WORK: &str = "\n\nDo the work through the workspace's apps. Before you end, write \
    what a later session needs into the note. If you have to wait (for a reply, a time, a \
    resource to change, a person), call Tasks' `wait` on this task with what you wait for, the \
    latest moment the wait ends and its kind, say in one line what you wait for, and end: you \
    are started again when it comes or at that moment. Where one of your person's automations \
    can see what you wait for (a time, a resource of the workspace changing), set it up first \
    and name its id in the wait as `on`: when it comes, the task goes on with what the \
    automation says and nothing else is asked. Remove an automation you set up only for a wait \
    once the wait is over. Where the work falls into pieces that can be done apart, make each a \
    subtask of this one with `add_subtask` and give it to yourself with `assign`, say with \
    `set_pieces_at_once` how many are in work at once, give each a budget with `set_budget`, \
    and `wait` on this task with a wait of the kind `pieces`: you are started again when they \
    are through. When the work is finished, move the task to In review with `set_status`, never \
    to Done: that is its person's to say. If you cannot finish, say plainly why.";

/// What a session is told of a task's work as it stood before the session began.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Before {
    /// How many sessions worked it before.
    pub sessions: i64,
    /// Whether the session before this one began and never ended.
    pub interrupted: bool,
    /// Its budget in money, in millionths; none for no limit.
    pub budget_micros: Option<u64>,
    /// What is left of that budget.
    pub money_left: Option<u64>,
    /// What made it go on after its latest wait, when something did.
    pub resumed: Option<String>,
}

impl Before {
    fn of_work(work: &tl_tasks::Work) -> Self {
        Self {
            sessions: work.sessions,
            interrupted: work.interrupted,
            budget_micros: work
                .budget_micros
                .map(|budget| u64::try_from(budget).unwrap_or_default()),
            money_left: work.money_left(),
            resumed: work.resumed.clone(),
        }
    }

    fn of_issue(work: &Work) -> Self {
        Self {
            sessions: i64::try_from(work.sessions).unwrap_or(i64::MAX),
            interrupted: work.session_since_ms.is_some(),
            budget_micros: work.budget_micros,
            money_left: work.money_left(),
            resumed: None,
        }
    }
}

/// One task's work, as a turn its agent was triggered to take on it reaches it.
pub(super) struct TaskWork {
    pub(super) service: super::WorkspaceActionProvider,
    /// The agent the turn is of, acting for the person it answers.
    pub(super) caller: Caller,
    /// The store's reads, decided for that agent and person.
    pub(super) reads: Arc<dyn ResourceReads>,
    pub(super) workspace: String,
    /// The task, by its id in Tasks.
    pub(super) task: String,
}

impl TaskWork {
    fn reader(&self) -> Reader<'_> {
        Reader::Turn {
            reads: Arc::clone(&self.reads),
            caller: &self.caller,
        }
    }

    /// Invoke Tasks' action `action` on the task, as the agent.
    async fn verb(&self, action: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        tl_tasks::invoke(
            &self.service,
            &self.caller,
            &self.workspace,
            &tl_tasks::resource(&self.task),
            action,
            arguments,
        )
        .await
    }

    /// Begin a session on the task: Tasks is told it began, its place is made when it has none,
    /// and `request` is told what the session needs. What its work kept before the session;
    /// none for a task this turn cannot read.
    ///
    /// # Errors
    /// Tasks refused the session: its plain line, which the task's work is stopped with.
    pub(super) async fn begin(&self, request: &mut String) -> Result<Option<Before>, String> {
        let reader = self.reader();
        let Some(task) = reader.task(&self.workspace, &self.task).await? else {
            return Ok(None);
        };
        let work = reader.work(&self.workspace, &task).await?;
        if let Err(words) = self.verb("session_began", Map::new()).await {
            self.stopped("failed", &words).await;
            return Err(words);
        }
        let place = self.place(&task, &work).await;
        let before = Before::of_work(&work);
        *request = told(request, &before, &place);
        Ok(Some(before))
    }

    /// The task's place, made when it has none: its folder's path and its note, kept on the
    /// task through Tasks' `set_place`. A note that cannot be made leaves the place with its
    /// folder alone, and the turn is told so; a folder that cannot be made keeps its name, and
    /// the turn is told it is not there.
    async fn place(&self, task: &tl_tasks::Task, work: &tl_tasks::Work) -> Place {
        let folder = if work.folder.is_empty() {
            folder_of(task.number, &task.title)
        } else {
            work.folder.clone()
        };
        let mut note = work.note.clone();
        let mut no_note = None;
        if note.is_empty() {
            // Tasks writes the note from the task, so it reads under it.
            match self.verb("write_note", Map::new()).await.and_then(|made| {
                made.pointer("/resources/0")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("Tasks made no note: {made}"))
            }) {
                Ok(made) => note = made,
                Err(error) => {
                    tracing::warn!(workspace = %self.workspace, task = %self.task, %error, "a task's note is not made");
                    no_note = Some(error);
                }
            }
        }
        if work.folder != folder || work.note != note {
            let mut arguments = Map::new();
            arguments.insert("folder".into(), json!(folder));
            arguments.insert("note".into(), json!(note));
            if let Err(error) = self.verb("set_place", arguments).await {
                tracing::warn!(workspace = %self.workspace, task = %self.task, %error, "a task's place is not kept");
            }
        }
        let no_folder = make_folder(&self.service, &self.caller, &self.workspace, &folder)
            .await
            .err();
        if let Some(error) = &no_folder {
            tracing::warn!(workspace = %self.workspace, task = %self.task, %error, "a task's folder is not made");
        }
        Place {
            folder,
            no_folder,
            note,
            no_note,
            text: None,
        }
    }

    /// Count the session that ended as `outcome`: what it cost and how long it worked. One that
    /// could not finish is written on the task as stopped, and one that ran out of the task's
    /// own money says that rather than the day's limit: the outcome its turn ends with.
    pub(super) async fn ended_as(
        &self,
        outcome: Outcome,
        cost_micros: u64,
        worked: Duration,
        before: &Before,
    ) -> Outcome {
        let mut counted = Map::new();
        counted.insert(
            "spent_micros".into(),
            json!(i64::try_from(cost_micros).unwrap_or(i64::MAX)),
        );
        counted.insert(
            "worked_ms".into(),
            json!(i64::try_from(worked.as_millis()).unwrap_or(i64::MAX)),
        );
        if let Err(error) = self.verb("session_ended", counted).await {
            tracing::warn!(workspace = %self.workspace, task = %self.task, %error, "a session on a task is not counted");
        }
        let (why, outcome) = ended(outcome, cost_micros, before);
        if let Some((why, words)) = why {
            self.stopped(why, &words).await;
        }
        outcome
    }

    /// Write on the task that its work stopped, and why.
    async fn stopped(&self, why: &str, words: &str) {
        let mut arguments = Map::new();
        arguments.insert("why".into(), json!(why));
        arguments.insert("words".into(), json!(words));
        if let Err(error) = self.verb("stop", arguments).await {
            tracing::warn!(workspace = %self.workspace, task = %self.task, %error, "a task's stop is not written");
        }
    }
}

/// How a session that ended as `outcome` stops its task's work (why, by the case Tasks names,
/// and the plain line), and the outcome its turn ends with: one that ran out of the task's own
/// money says that rather than the day's limit.
fn ended(
    outcome: Outcome,
    cost_micros: u64,
    before: &Before,
) -> (Option<(&'static str, String)>, Outcome) {
    match outcome {
        Outcome::Answered(words) => (None, Outcome::Answered(words)),
        Outcome::Stopped(_) if before.money_left.is_some_and(|left| cost_micros >= left) => {
            let words = spent(before.budget_micros.unwrap_or_default());
            (Some(("budget", words.clone())), Outcome::Stopped(words))
        }
        Outcome::Stopped(words) => (Some(("failed", words.clone())), Outcome::Stopped(words)),
        Outcome::Failed(words) => (Some(("failed", words.clone())), Outcome::Failed(words)),
        Outcome::Cancelled => (
            Some(("person", "Its person stopped the work.".to_owned())),
            Outcome::Cancelled,
        ),
    }
}

/// The line of a task whose budget of `budget_micros` is spent.
fn spent(budget_micros: u64) -> String {
    format!(
        "I stopped: this task's budget of {} is spent. Raise it to let me go on.",
        money(budget_micros)
    )
}

/// Make the folder `folder` in the workspace's files, with the folders on the way to it:
/// Files' own `new-folder` by a path in the workspace, asked as `caller`, so with what its
/// rules allow it of Files. A folder that is there is left as it is: every session asks, none
/// makes a second one, and one its person took away is there again.
///
/// # Errors
/// The agent's rules refuse it Files, the path is none of the workspace, or the workspace did
/// not take the change: why the folder is not there, in words the turn is told.
async fn make_folder(
    service: &super::WorkspaceActionProvider,
    caller: &Caller,
    workspace: &str,
    folder: &str,
) -> Result<(), String> {
    let (parent, name) = folder.rsplit_once('/').unwrap_or(("", folder));
    let mut arguments = Map::new();
    arguments.insert("parent".into(), json!(parent));
    arguments.insert("name".into(), json!(name));
    let target = RemoteActionTarget {
        provider: workspace_provider(workspace),
        address: ActionAddress {
            resource: format!("{APPLICATION_RESOURCE_PREFIX}{FILES}"),
            action: "new-folder".to_owned(),
        },
    };
    super::invoke_at(service, caller, target, InvokeArgument::Json(arguments))
        .await
        .map(drop)
}

/// One issue's work, as a session of its agent reaches it.
pub(super) struct IssueWork {
    pub(super) service: super::WorkspaceActionProvider,
    /// The agent the issue is assigned to, on this turn of the person it works for.
    pub(super) caller: Caller,
    pub(super) workspace: String,
    /// The part of the tracker's issues that keeps the issue.
    pub(super) document: String,
    pub(super) issue: String,
}

/// What a session needs of an issue, taken from its document before anything is awaited: an
/// issue's rich text does not travel between threads, and a turn does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Kept {
    pub(super) number: u64,
    pub(super) title: String,
    /// What the issue keeps of its work; none until something used it.
    pub(super) work: Option<Work>,
}

impl Kept {
    pub(super) fn of(issue: &Issue) -> Self {
        Self {
            number: issue.number,
            title: issue.title.clone(),
            work: issue.work.clone(),
        }
    }
}

/// Where an issue's working material is, and what its note reads now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) folder: String,
    /// Why the folder could not be made, when it could not: it is named and is not there.
    pub(super) no_folder: Option<String>,
    /// The note's resource in Notes; empty when none could be made.
    pub(super) note: String,
    /// Why no note could be made, when none could.
    pub(super) no_note: Option<String>,
    /// The note as Markdown, cut to what a session is told; none when it could not be read.
    pub(super) text: Option<String>,
}

/// Why no session may begin on an issue that keeps `work`, with the plain line its person
/// reads: its budget is used up, or its sessions were lost too often in a row.
pub(super) fn refused(work: Option<&Work>) -> Option<(StoppedWhy, String)> {
    let work = work?;
    match work.used_up() {
        Some(StoppedWhy::Time) => {
            return Some((
                StoppedWhy::Time,
                "I stopped: the working time this task was given is used up. Give it more time \
                 to let me go on."
                    .to_owned(),
            ));
        }
        Some(why) => return Some((why, spent(work.budget_micros.unwrap_or_default()))),
        None => {}
    }
    (work.session_since_ms.is_some() && work.lost >= MOST_LOST_SESSIONS).then(|| {
        (
            StoppedWhy::LostWorker,
            format!(
                "I stopped: my work on this was interrupted {} times in a row before it ended. \
                 Tell me to go on and I start again from its notes.",
                work.lost.saturating_add(1)
            ),
        )
    })
}

/// The folder a task's material is kept in when nobody named one: `Tasks/<number> <title>`,
/// the title without what a folder may not be called.
fn folder_of(number: i64, title: &str) -> String {
    let title: String = title
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
        format!("Tasks/{number}")
    } else {
        format!("Tasks/{number} {title}")
    }
}

impl IssueWork {
    fn route(&self, app: &str, document: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: self.workspace.clone(),
            document: document.to_owned(),
        }
    }

    /// Invoke Tasks' verb `action` on the issue, as the agent.
    async fn verb(&self, action: &str, mut arguments: Map<String, Value>) -> Result<Value, String> {
        arguments.insert("issue".into(), json!(self.issue));
        invoke_named(
            &self.service,
            &self.caller,
            &self.route(TASKS, &self.document),
            &tasks_model::issue_resource(&tilo_resource::Id::from_raw(self.issue.clone())),
            action,
            arguments,
        )
        .await
    }

    /// Say an agent session began: the issue no longer waits and is no longer stopped.
    pub(super) async fn began(&self) -> Result<(), String> {
        self.verb("session-began", Map::new()).await.map(drop)
    }

    /// Count the session that ended as `outcome`: what it cost and how long it worked. One that
    /// could not finish is written on the issue as stopped, and one that ran out of the issue's
    /// own money says that rather than the day's limit: the outcome its thread is told.
    pub(super) async fn ended_as(
        &self,
        outcome: Outcome,
        cost_micros: u64,
        worked: Duration,
        before: &Work,
    ) -> Outcome {
        let mut counted = Map::new();
        counted.insert("cost_micros".into(), json!(cost_micros));
        counted.insert(
            "worked_ms".into(),
            json!(u64::try_from(worked.as_millis()).unwrap_or(u64::MAX)),
        );
        if let Err(error) = self.verb("session-ended", counted).await {
            tracing::warn!(workspace = %self.workspace, issue = %self.issue, %error, "a session on an issue is not counted");
        }
        let (why, outcome) = ended(outcome, cost_micros, &Before::of_issue(before));
        if let Some((why, words)) = why {
            self.stop(json!(why), &words).await;
        }
        outcome
    }

    /// Write on the issue that its work stopped, and why.
    pub(super) async fn stopped(&self, why: StoppedWhy, words: &str) {
        self.stop(json!(why), words).await;
    }

    /// Write on the issue that its work stopped as `why`, by the case Tasks names.
    async fn stop(&self, why: Value, words: &str) {
        let mut arguments = Map::new();
        arguments.insert("why".into(), why);
        arguments.insert("words".into(), json!(words));
        if let Err(error) = self.verb("stop", arguments).await {
            tracing::warn!(workspace = %self.workspace, issue = %self.issue, %error, "an issue's stop is not written");
        }
    }

    /// The issue's place, made when it has none: its folder's path and its note, kept on the
    /// issue, and what the note reads now. A note that cannot be made leaves the place with its
    /// folder alone, and the turn is told so. The folder is made in the workspace's files
    /// before every session ([`Self::make_folder`]); one that cannot be made keeps its name,
    /// and the turn is told it is not there.
    pub(super) async fn place(&self, workspaces: &Workspaces, issue: &Kept) -> Place {
        let kept = issue.work.clone().unwrap_or_default();
        let folder = if kept.folder.is_empty() {
            folder_of(
                i64::try_from(issue.number).unwrap_or(i64::MAX),
                &issue.title,
            )
        } else {
            kept.folder.clone()
        };
        let mut note = kept.note.clone();
        let mut no_note = None;
        if note.is_empty() {
            match self.make_note(workspaces, issue).await {
                Ok(made) => note = made,
                Err(error) => {
                    tracing::warn!(workspace = %self.workspace, issue = %self.issue, %error, "an issue's note is not made");
                    no_note = Some(error);
                }
            }
        }
        if kept.folder != folder || kept.note != note {
            let mut arguments = Map::new();
            arguments.insert("folder".into(), json!(folder));
            if !note.is_empty() {
                arguments.insert("note".into(), json!(note));
            }
            if let Err(error) = self.verb("set-place", arguments).await {
                tracing::warn!(workspace = %self.workspace, issue = %self.issue, %error, "an issue's place is not kept");
            }
        }
        let no_folder = make_folder(&self.service, &self.caller, &self.workspace, &folder)
            .await
            .err();
        if let Some(error) = &no_folder {
            tracing::warn!(workspace = %self.workspace, issue = %self.issue, %error, "an issue's folder is not made");
        }
        let text = if note.is_empty() {
            None
        } else {
            self.read_note(&note)
        };
        Place {
            folder,
            no_folder,
            note,
            no_note,
            text,
        }
    }

    /// Make the issue's note in Notes, titled after it, and link it from the issue: the note's
    /// resource.
    ///
    /// Notes is a tilolang package: its notebook is one record of its own, made by its `new_note`
    /// when the workspace first asks for one, and a note is a `tl:so.tilo.notes:<id>` resource in
    /// it. A workspace's server runs the package where its host does; where it does not, the call
    /// is refused and the session is told why instead of working on in silence.
    async fn make_note(&self, _workspaces: &Workspaces, issue: &Kept) -> Result<String, String> {
        // The agent opens Notes in the workspace, as it does with its own tools: an app's
        // actions are offered to a session once it opened the app.
        let opened = super::invoke_at(
            &self.service,
            &self.caller,
            tilo_actions::remote::application_open_target(&self.workspace, NOTES),
            tilo_actions::remote::InvokeArgument::None,
        )
        .await;
        if let Err(error) = opened {
            tracing::debug!(workspace = %self.workspace, %error, "Notes did not open for an issue's note");
        }
        let title = format!("#{} {}", issue.number, issue.title);
        let mut arguments = Map::new();
        arguments.insert("title".into(), json!(title));
        arguments.insert("body".into(), json!(NOTE_STARTS));
        let made = invoke_named(
            &self.service,
            &self.caller,
            &self.route(NOTES, ""),
            &format!("tl:{NOTES}"),
            "new_note",
            arguments,
        )
        .await?;
        let note = made
            .pointer("/resources/0")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("Notes made no note: {made}"))?
            .to_owned();
        let mut link = Map::new();
        link.insert("app".into(), json!(NOTES));
        link.insert("kind".into(), json!(format!("tl.{NOTES}.Note")));
        link.insert("resource".into(), json!(note));
        link.insert("label".into(), json!(title));
        if let Err(error) = self.verb("link", link).await {
            tracing::warn!(workspace = %self.workspace, issue = %self.issue, %error, "an issue's note is not linked from it");
        }
        Ok(note)
    }

    /// What the note reads now, as Markdown, cut to what a session is told.
    ///
    /// Not read yet: the workspace's action answer says the resources an action made and never
    /// the value a tilolang action answered, so a note's text (`export_markdown`) has no way
    /// back to the server. A session writes the note and works from what it wrote; a later one
    /// is told there is nothing more to read rather than being told wrongly.
    fn read_note(&self, _note: &str) -> Option<String> {
        tracing::warn!(
            workspace = %self.workspace,
            issue = %self.issue,
            "an issue's note is not read: a tilolang action's answered value does not travel in \
             the workspace's action answer"
        );
        None
    }
}

/// What an agent is told of the issue `ask` gives it: the issue, and what asked.
pub(super) fn asked(issue: &Issue, ask: &IssueAsk) -> String {
    let mut request = format!(
        "Issue #{} \"{}\" in Tasks (`{}`) is assigned to you.",
        issue.number,
        issue.title,
        tasks_model::issue_resource(&ask.issue)
    );
    let body = issue.body.text();
    if !body.trim().is_empty() {
        request.push_str("\n\nIts description:\n");
        request.push_str(body.trim());
    }
    match ask.asked {
        Asked::Message if !ask.words.trim().is_empty() => {
            request.push_str("\n\nThe newest comment for you:\n");
            request.push_str(ask.words.trim());
        }
        Asked::Resumed => {
            request.push_str("\n\nYou are told to go on with it: ");
            request.push_str(ask.words.trim());
        }
        Asked::WaitEnded => request.push_str(
            "\n\nYou let it wait, and what you waited for did not come by the moment you set. \
             The wait is over: decide how to go on.",
        ),
        Asked::PiecesEnded => {
            request.push_str("\n\nThe pieces you let it wait for are through (");
            request.push_str(ask.words.trim());
            request.push_str("). Read what each left in its thread and its note, and go on.");
        }
        Asked::Assigned | Asked::Message => {}
    }
    request
}

/// `request` with what a session on an issue needs beside the ask ([`told`]), `work` what the
/// issue kept before this session.
pub(super) fn with_place(request: &str, work: &Work, place: &Place) -> String {
    told(request, &Before::of_issue(work), place)
}

/// `request` with what a session needs beside the ask: what made the task go on, that earlier
/// sessions happened, where its working material is and what its note reads now, what is left
/// of its budget, and how to wait and how to end.
fn told(request: &str, work: &Before, place: &Place) -> String {
    let mut request = request.to_owned();
    if let Some(cause) = work
        .resumed
        .as_deref()
        .filter(|cause| !cause.trim().is_empty())
    {
        request.push_str("\n\nIt waited, and goes on now: ");
        request.push_str(cause.trim());
        request.push('.');
    }
    if work.sessions > 0 {
        request.push_str(
            "\n\nThis is not your first session on it, and you remember nothing of the earlier \
             ones: everything you know of them is in its note, its folder and its thread.",
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
             needs in the issue's thread."
        );
        request.push_str(&none);
    } else {
        let note = format!(
            "\n- Its note in Notes (`{}`): keep the plan, what you found, each finding with where \
             it comes from, and what is still open there. Add to it or rewrite it as you go.",
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
             Put no files there; keep what a later session needs in its note or its thread.",
            place.folder
        ),
    };
    request.push_str(&folder);
    if let Some(text) = place.text.as_deref().filter(|text| !text.is_empty()) {
        request.push_str("\n\nThe note as it reads now:\n");
        request.push_str(text);
    }
    if let Some(left) = work.money_left {
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

#[cfg(all(test, feature = "apps"))]
mod tests {
    use serde_json::json;
    use tilo_actions::remote::ActionProvider as _;

    use super::*;
    use crate::agents::tl_tasks::testing::{Broker, Host};

    /// A workspace whose Tasks holds the task "Book the venue", its work as its person's
    /// `before` actions on it left it, and a turn of the agent on it through a broker that
    /// denies `refuses`.
    async fn worked(
        before: &[(&str, serde_json::Value)],
        refuses: &[(&'static str, &'static str)],
    ) -> (Host, TaskWork, Arc<Broker>) {
        let host = Host::new().await;
        let id = host
            .new_task("Book the venue", json!({"assignee": {"agent": "scout"}}))
            .await;
        for (action, arguments) in before {
            host.act(&tl_tasks::resource(&id), action, arguments.clone())
                .await;
        }
        let broker = host.broker(refuses);
        let service = host.service(&broker);
        let task = TaskWork {
            reads: service.resource_reads().unwrap(),
            service,
            caller: host.caller(),
            workspace: host.workspace.clone(),
            task: id,
        };
        (host, task, broker)
    }

    /// What the task's work holds now, as the turn reads it.
    async fn work_of(task: &TaskWork) -> tl_tasks::Work {
        let reader = task.reader();
        let read = reader
            .task(&task.workspace, &task.task)
            .await
            .unwrap()
            .unwrap();
        reader.work(&task.workspace, &read).await.unwrap()
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
    /// Tasks' words, which stop the task's work, and no place is made for it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_tasks_refuses_ends_with_its_words_and_makes_nothing() {
        const SPENT: &str = "I stopped: this task's budget is spent. Raise it to let me go on.";
        let (host, task, broker) = worked(
            &[
                ("set_budget", json!({"micros": 10})),
                ("session_began", json!({})),
                ("session_ended", json!({"spent_micros": 10})),
            ],
            &[],
        )
        .await;
        let mut request = "Book the venue".to_owned();
        let refused = task.begin(&mut request).await.unwrap_err();
        assert!(refused.contains(SPENT), "{refused}");
        assert_eq!(request, "Book the venue", "the turn is told nothing more");
        assert_eq!(actions(&broker), ["session_began", "stop"]);
        let stop = broker.asked.lock().unwrap()[1].3.clone();
        assert_eq!(stop["why"], "failed");
        assert!(stop["words"].as_str().unwrap().contains(SPENT));
        host.end().await;
    }

    /// A session that begins is told what made its task go on, that sessions came before, what
    /// is left of its budget and where its material is, made for it through Tasks and Files as
    /// the agent; one that runs out of the task's money stops the task's work as spent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_is_told_its_tasks_work_and_counted_when_it_ends() {
        let (host, task, broker) = worked(
            &[
                ("set_budget", json!({"micros": 1_000_000})),
                ("session_began", json!({})),
                ("session_ended", json!({"spent_micros": 125_000})),
                ("session_began", json!({})),
                ("session_ended", json!({"spent_micros": 125_000})),
                ("wait", json!({"what": "the venue's reply"})),
                ("resume", json!({"cause": "the pieces are through"})),
            ],
            &[],
        )
        .await;
        let mut request = "Book the venue".to_owned();
        let before = task.begin(&mut request).await.unwrap().unwrap();
        assert_eq!(before.money_left, Some(750_000));
        assert!(
            request.contains("goes on now: the pieces are through."),
            "{request}"
        );
        assert!(request.contains("not your first session"), "{request}");
        assert!(request.contains("$0.75 of $1.00 is left"), "{request}");
        assert!(request.contains("`tl:so.tilo.notes:"), "{request}");
        assert!(request.contains("`Tasks/1 Book the venue`"), "{request}");
        let work = work_of(&task).await;
        assert!(work.note.starts_with("tl:so.tilo.notes:"), "{work:?}");
        assert_eq!(work.folder, "Tasks/1 Book the venue");
        assert_eq!(work.sessions, 3);
        assert!(work.interrupted, "the session is under way");
        assert_eq!(
            actions(&broker),
            ["session_began", "write_note", "set_place", "new-folder"]
        );
        let ended = task
            .ended_as(
                Outcome::Stopped("the model ran out".into()),
                750_000,
                Duration::from_secs(3),
                &before,
            )
            .await;
        assert!(
            matches!(&ended, Outcome::Stopped(words) if words.contains("budget of $1.00 is spent")),
            "{ended:?}"
        );
        let asked = broker.asked.lock().unwrap().clone();
        let (_, counted, stop) = (&asked[4], &asked[4].3, &asked[5].3);
        assert_eq!(asked[4].2, "session_ended");
        assert_eq!(counted["spent_micros"], 750_000);
        assert_eq!(counted["worked_ms"], 3_000);
        assert_eq!(asked[5].2, "stop");
        assert_eq!(stop["why"], "budget");
        let work = work_of(&task).await;
        assert_eq!(work.spent_micros, 1_000_000);
        assert_eq!(work.money_left(), Some(0));
        assert!(!work.interrupted);
        // Every write is the agent's own, on the task it works.
        for (caller, resource, action, _) in &asked {
            assert_eq!(caller, &task.caller, "{action}");
            if action != "new-folder" {
                assert_eq!(resource, &tl_tasks::resource(&task.task), "{action}");
            }
        }
        host.end().await;
    }

    /// An agent the broker lets write nothing on the task begins no session on it, and nothing
    /// is asked as anyone else: the turn ends with the broker's refusal.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_without_a_grant_on_the_task_writes_nothing_to_it() {
        const DENIED: &str = "the agent may not change this task";
        let (host, task, broker) = worked(
            &[],
            &[
                ("session_began", DENIED),
                ("stop", DENIED),
                ("write_note", DENIED),
                ("set_place", DENIED),
            ],
        )
        .await;
        let mut request = "Book the venue".to_owned();
        let refused = task.begin(&mut request).await.unwrap_err();
        assert!(refused.contains(DENIED), "{refused}");
        assert_eq!(actions(&broker), ["session_began", "stop"]);
        assert!(
            broker
                .asked
                .lock()
                .unwrap()
                .iter()
                .all(|(caller, ..)| caller == &task.caller)
        );
        host.end().await;
    }
}
