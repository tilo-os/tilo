//! What a workspace's server does for an issue that is worked on over many agent sessions.
//!
//! An issue given to an agent may take hours or weeks: sessions with waits between them, on a
//! server that restarts. Everything a later session needs is kept with the issue
//! ([`tasks_model::Work`]) and in the workspace's own Notes and Files, never beside them, so this
//! module keeps nothing: it reads the issue, and writes to it through Tasks' own verbs as the
//! agent whose session it is, for the person the agent works for.
//!
//! - Before a session it makes the issue's place when it has none (a note in Notes, linked from
//!   the issue, and a folder named for the issue), says the session began, and tells it what the
//!   note reads now. The folder is made in the workspace's files by Files' own `new-folder`, as
//!   the agent on its turn: an agent whose rules refuse it Files has none, and its session is
//!   told so.
//! - After a session it counts what the session cost and how long it worked.
//! - A session that could not finish (the issue's budget, the day's limit, a failure) is written
//!   on the issue as stopped, with its plain line. Nothing here writes `done`.

use std::time::Duration;

use serde_json::{Map, Value, json};
use tasks_model::{Asked, Issue, IssueAsk, MOST_LOST_SESSIONS, StoppedWhy, Work};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{
    APPLICATION_RESOURCE_PREFIX, Caller, InvokeArgument, RemoteActionTarget, workspace_provider,
};
use tilo_peer_state::document_frame::DocumentRoute;

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

/// How a session on an issue works and ends, as every one is told.
const HOW_TO_WORK: &str = "\n\nDo the work through the workspace's apps. Move the issue to In \
    progress when you start. Before you end, write what a later session needs into the note. \
    If you have to wait (for a reply, a time, a resource to change, a person), call Tasks' `wait` \
    on this issue with what you wait for and the latest moment the wait ends, say in one line \
    what you wait for, and end: you are started again when it comes or at that moment. Where \
    one of your person's automations can see what you wait for (a time, a resource of the \
    workspace changing), set it up first and name its id in the wait as `event`: when it comes, \
    the task goes on with what the automation says and nothing else is asked. Remove an \
    automation you set up only for a wait once the wait is over. Where the work falls into \
    pieces that can be done apart, make each an issue under this one and give it to yourself, \
    say with `set-pieces-at-once` how many are in work at once, give each a budget, and `wait` \
    on this issue with a wait of the kind `pieces`: you are started again when they are \
    through. When the \
    work is finished, move the issue to In review, never to Done: that is its person's to say. \
    If you cannot finish, say plainly why.";

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
        Some(why) => return Some((why, spent_line(work))),
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

/// The line of an issue whose budget in money is spent.
fn spent_line(work: &Work) -> String {
    format!(
        "I stopped: this task's budget of {} is spent. Raise it to let me go on.",
        money(work.budget_micros.unwrap_or_default())
    )
}

/// The folder an issue's material is kept in when nobody named one: `Tasks/<number> <title>`,
/// the title without what a folder may not be called.
fn folder_of(issue: &Kept) -> String {
    let title: String = issue
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
        format!("Tasks/{}", issue.number)
    } else {
        format!("Tasks/{} {title}", issue.number)
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
        match outcome {
            Outcome::Answered(words) => Outcome::Answered(words),
            Outcome::Stopped(_) if before.money_left().is_some_and(|left| cost_micros >= left) => {
                let words = spent_line(before);
                self.stopped(StoppedWhy::Budget, &words).await;
                Outcome::Stopped(words)
            }
            Outcome::Stopped(words) => {
                self.stopped(StoppedWhy::Failed, &words).await;
                Outcome::Stopped(words)
            }
            Outcome::Failed(words) => {
                self.stopped(StoppedWhy::Failed, &words).await;
                Outcome::Failed(words)
            }
            Outcome::Cancelled => {
                self.stopped(StoppedWhy::Person, "Its person stopped the work.")
                    .await;
                Outcome::Cancelled
            }
        }
    }

    /// Write on the issue that its work stopped, and why.
    pub(super) async fn stopped(&self, why: StoppedWhy, words: &str) {
        let mut arguments = Map::new();
        arguments.insert("why".into(), json!(why));
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
            folder_of(issue)
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
        let no_folder = self.make_folder(&folder).await.err();
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
        super::invoke_at(&self.service, &self.caller, target, asked)
            .await
            .map(drop)
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

/// `request` with what a session needs beside the ask: that earlier ones happened (`work` is what
/// the issue kept before this one), where its working material is and what its note reads now,
/// what is left of its budget, and how to wait and how to end.
pub(super) fn with_place(request: &str, work: &Work, place: &Place) -> String {
    let mut request = request.to_owned();
    if work.sessions > 0 {
        request.push_str(
            "\n\nThis is not your first session on it, and you remember nothing of the earlier \
             ones: everything you know of them is in its note, its folder and its thread.",
        );
    }
    if work.session_since_ms.is_some() {
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
