//! Terminal sessions the host runs: a hosted workspace's shell lives with the
//! workspace, beside the host's VMs.
//!
//! On a device, a Terminal session record's `run` queues the command line
//! for the process running the session, which types it at its shell and
//! records the command it became (`record-command`). A workspace the host serves has
//! no such process on any device: the host is where its sessions run. So a
//! person's device (an agent on one of their computers among them) asking
//! the host for a session's `run` has it run here, by the host's
//! [`WorkspaceShell`], in that session's shell (an agent's command in a shell of its
//! own, which keeps its directory between commands), and the host records the
//! command on the session record through the record's own `record-command`, under
//! the invocation's context: History keeps it as the agent's command, with its
//! command, directory, exit status and output.
//!
//! Whoever the workspace lets run the session's commands has them run here:
//! the host's own person and their agents, and a member or a member's agent
//! the workspace's policy admits for the session's `run`, decided as every
//! action the host runs is ([`Worker::terminal_admitted`]). A command runs in
//! the workspace's own environment, a VM when it needs one, billed to the
//! workspace's account; where that account cannot pay yet, the invocation answers
//! with the host's refusal and what would let it through. The invocation is
//! recorded as the caller's, for the person they act for.
//!
//! A command may run for minutes (a build in the workspace's VM), so it runs
//! on a thread of its own: the invocation is answered accepted at once, and
//! succeeds with the command once it ended and was recorded. Its answer is what
//! Terminal answers a `run` with, plus what an agent needs without a second
//! call: the command line, the directory it ran in and what it printed.
//!
//! While it runs, the invocation says what the command printed so far
//! ([`Watching::seen`]), so an agent following a long build is not silent;
//! and it can be stopped: a person's Ctrl+C in the session, or an agent's
//! cancel of its invocation, ends the command in the host's shell
//! ([`Watching::stop`]), records the command like any other, and settles the
//! invocation as cancelled.
//!
//! A person typing in the session needs its shell on a terminal, as on a device: the
//! session's `open-terminal` has the host start that shell on a terminal
//! ([`WorkspaceShell::open`]) and hand its stream to the device that asked, once the same
//! admission `run` passes let it through (a person whose open is asked about is asked once,
//! never per key). It succeeds with the stream's one-use id, which that device alone presents
//! when it dials the host's terminal streams, and the session's id, which a later
//! `open-terminal` names (`attach`) to come back to it.
//! Each command line the shell runs there is recorded on the session as `run`'s commands are
//! (one record path, [`Worker::record_ended`]), under the open's invocation, whether a device
//! is attached or not: the host is the one that records them, so the terminal's stream marks
//! no runs for a device to record again.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::{Map, Value, json};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::environment_commands::{CommandOutput, Person};
use tilo_actions::remote::server_account::Refusal;
use tilo_actions::remote::{Caller, InvocationReceipt, InvocationState, InvokeRequest};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;

use super::{Job, Needs, Shared, Worker, caller_context, invoke_arguments, take_all};
use crate::record_actions::resource_of;
use tilo_server_protocol::WorkspaceId;

/// Terminal's app id: its session records are what the host runs.
pub use ui::apps::TERMINAL as TERMINAL_APP;
/// The session record's action that runs a command line.
const RUN: &str = "run";
/// The session record's action that stops the command line it is running.
const STOP: &str = "stop";
/// The session record's action that opens its shell on a terminal for the caller's device.
const OPEN: &str = "open-terminal";
/// The window a terminal opens at when the caller names none.
const DEFAULT_SIZE: (u16, u16) = (80, 24);
/// The widest and tallest window a terminal opens at.
const MAX_SIDE: u16 = 1000;
/// The session record's action that records a command that ended.
const RECORD_COMMAND: &str = "record-command";
/// The longest command line a session runs, as Terminal takes it.
const MAX_COMMAND_BYTES: usize = 4096;
/// What an answer carries of a command's output at most: its end, where a
/// build's errors and a test summary are.
const ANSWERED_OUTPUT: usize = 32 * 1024;

/// What the caller of a command sees of it while it runs, and how it stops it.
#[derive(Clone, Copy, Default)]
pub struct Watching<'a> {
    /// What a command printed so far, each time that grew.
    pub seen: Option<&'a (dyn Fn(&CommandOutput) + Sync)>,
    /// Whether the caller asked the command to stop.
    pub stop: Option<&'a (dyn Fn() -> bool + Sync)>,
}

/// Where a host's workspaces run their shells.
pub trait WorkspaceShell: Send + Sync {
    /// Run `command` in `workspace`'s Terminal session `session` for `by`, who asked; `agent`
    /// when an agent asked, whose output is rendered for it and whose
    /// refused program is not waited for. `person` is who it runs as, whose
    /// name and address the workspace's VM programs record as the author of
    /// what they make. `watching` shows the caller what the command prints
    /// while it runs, and stops it when the caller asks.
    ///
    /// # Errors
    ///
    /// The session's shell could not be started.
    fn run(
        &self,
        workspace: &str,
        session: &str,
        command: &str,
        by: &Caller,
        agent: bool,
        person: Option<&Person>,
        watching: Watching<'_>,
    ) -> Result<ShellRan, String>;

    /// `caller` was let run commands in `workspace`'s session `session`: the person its
    /// programs' agents ask whether they may work in the workspace.
    fn started(&self, workspace: &str, session: &str, caller: &Caller) {
        let _ = (workspace, session, caller);
    }

    /// Start `opening`'s session shell on a terminal for `by`, who asked, and hand its stream
    /// to the device it names.
    ///
    /// # Errors
    ///
    /// This host opens no terminals, the session it names to come back to is gone, or the
    /// shell could not start.
    ///
    /// Each command line the shell runs from then on goes to `record` as it ends, attached or
    /// not, when this open started the shell; an open that came back to it records nothing
    /// more, its runs already going to the first.
    fn open(&self, opening: &Opening, by: &Caller, record: Recorder) -> Result<Opened, String> {
        let _ = (opening, by, record);
        Err("this host opens no terminals".into())
    }

    /// Keep `output`, a command's output, where the workspace's devices fetch
    /// its content, answering the reference that opens it, or `None` when
    /// this host keeps none.
    fn keep(&self, workspace: &str, output: &[u8]) -> Option<String> {
        let _ = (workspace, output);
        None
    }
}

/// Who a command `caller` asks for in `workspace` runs as: an agent by its own name, else the
/// person it acts for, as `names` (the workspace's member list) calls them. `host` is the
/// address the workspace is reached by, which their address is at (`<name>@<host>`). `None`
/// when the caller is neither a person nor an agent the workspace names.
///
/// Someone the list does not name, or names with no word, still signs: as a Tilo actor, never
/// as a key, since what a commit records of its author is read by people.
#[must_use]
pub fn person_of(
    caller: &Caller,
    names: &[(tilo_actions::MemberId, String)],
    host: &str,
) -> Option<Person> {
    /// The word `names` calls the member `id`, when it calls it one.
    fn named<'a>(names: &'a [(tilo_actions::MemberId, String)], id: &str) -> Option<&'a str> {
        names
            .iter()
            .find(|(member, name)| member.id() == id && !name.trim().is_empty())
            .map(|(_, name)| name.as_str())
    }
    let acting = caller.acting();
    if let Some(agent) = acting
        .member_id()
        .and_then(tilo_actions::MemberId::agent_id)
    {
        let name = named(names, agent)
            .or_else(|| caller.agent_name().filter(|name| !name.trim().is_empty()))
            .unwrap_or("Tilo agent");
        return Some(Person::of(name, host));
    }
    let person = acting
        .person_id()
        .filter(|id| !id.is_empty())
        .or_else(|| acting.acted_for_person())?;
    let name = named(names, person).unwrap_or("Tilo member");
    Some(Person::of(name, host))
}

/// A Terminal session's shell to open on a terminal, for one device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opening {
    pub workspace: String,
    /// The session whose shell it is: the one its commands run in.
    pub session: String,
    /// The device that takes its stream: the caller's own.
    pub device: [u8; 32],
    pub cols: u16,
    pub rows: u16,
    /// Who the shell runs as: the person or agent who opened it, as the workspace names them.
    pub person: Option<Person>,
    /// The session to come back to, by the id an earlier open answered.
    pub attach: Option<String>,
}

/// What records each command line a terminal's shell ran, as it ends.
pub type Recorder = Arc<dyn Fn(ShellRan) + Send + Sync>;

/// An opened terminal: the stream's one-use id, for the device that asked, and the session's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub stream: String,
    /// 32 hex characters.
    pub session: String,
}

/// How a command line went in a host's shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellRan {
    pub command: String,
    pub cwd: String,
    pub exit: i32,
    pub output: Vec<u8>,
    pub truncated: bool,
    /// It was stopped before it ended: a person's Ctrl+C, or an agent's
    /// cancel of the command it asked for.
    pub cancelled: bool,
    pub started_ms: i64,
    pub ended_ms: i64,
    /// Why the host would not run a program the command needed, typed.
    pub refusal: Option<Refusal>,
}

/// A command that ended, for the worker to record.
pub(super) struct Ended {
    pub(super) workspace_id: WorkspaceId,
    pub(super) invocation: InvocationId,
    pub(super) route: DocumentRoute,
    pub(super) request: InvokeRequest,
    /// Who asked for it: the invocation its record's change is part of.
    pub(super) caller: Caller,
    pub(super) ran: Result<ShellRan, String>,
}

/// A terminal open that ended, for the worker to answer.
pub(super) struct OpenEnded {
    pub(super) workspace_id: WorkspaceId,
    pub(super) request: InvokeRequest,
    pub(super) caller: Caller,
    pub(super) opened: Result<Opened, String>,
}

/// A Terminal session the host runs that the caller was let act on, with what they asked.
struct Hosted<T> {
    shell: Arc<dyn WorkspaceShell>,
    workspace: String,
    route: DocumentRoute,
    asked: T,
}

/// One command asked of the host's shell, ready to start on a thread of its own.
struct Started {
    shell: Arc<dyn WorkspaceShell>,
    jobs: super::Jobs,
    shared: Arc<Shared>,
    workspace: String,
    session: String,
    command: String,
    agent: bool,
    /// Who the command runs as: the person the workspace names it by.
    person: Option<Person>,
    /// What a cancel of the command, or a Ctrl+C in its session, sets.
    stop: Arc<AtomicBool>,
    ended: Ended,
}

impl Started {
    /// Run the command, showing the invocation what it prints as it prints
    /// it, and hand the command to the worker to record once it ended.
    fn spawn(self) -> std::io::Result<()> {
        let Self {
            shell,
            jobs,
            shared,
            workspace,
            session,
            command,
            agent,
            person,
            stop,
            ended,
        } = self;
        let invocation = ended.invocation;
        // What the command prints as it prints it, where the invocation's
        // answer is read: an agent following a long build sees the newest
        // output rather than silence.
        let printing = move |printed: &CommandOutput| {
            let mut log = shared.log.lock_recover();
            if let Some(entry) = log.entries.get_mut(&invocation) {
                let sequence = match &entry.update {
                    InvocationUpdate::Item { sequence, .. } => sequence.saturating_add(1),
                    _ => 0,
                };
                entry.update = InvocationUpdate::Item {
                    invocation,
                    sequence,
                    value: serde_json::to_value(printed).unwrap_or_default(),
                };
            }
            drop(log);
        };
        std::thread::Builder::new()
            .name("workspace-shell".into())
            .spawn(move || {
                let stopping = || stop.load(Ordering::SeqCst);
                let watching = Watching {
                    seen: Some(&printing),
                    stop: Some(&stopping),
                };
                let ran = shell.run(
                    &workspace,
                    &session,
                    &command,
                    &ended.caller,
                    agent,
                    person.as_ref(),
                    watching,
                );
                let _ = jobs.send((Instant::now(), Job::Ran(Box::new(Ended { ran, ..ended }))));
            })
            .map(drop)
    }
}

impl Worker {
    /// `request`, when it is `action` on a Terminal session this host runs that `caller` may
    /// take, with its arguments as `read` takes them; `None` for any other request, and the
    /// receipt that answers it when it is refused or waits for the person.
    #[allow(
        clippy::result_large_err,
        reason = "the refusal is the receipt the invocation is answered with"
    )]
    fn hosted_session<T>(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        action: &str,
        read: impl FnOnce(&Map<String, Value>) -> Result<T, String>,
    ) -> Option<Result<Hosted<T>, InvocationReceipt>> {
        let invocation = request.invocation;
        if request.target.address.action != action {
            return None;
        }
        let route = resource_of(&request.target.provider, "")
            .filter(|route| route.app_id == TERMINAL_APP)?;
        // Unknown and forbidden are answered alike, so why the host turned the request away
        // is said here, where whoever runs it reads it.
        if !self.acts_for_owner(caller) && self.bounded(caller).is_none() {
            tracing::info!(
                %workspace_id,
                action,
                acting = ?caller.acting(),
                "a Terminal request is answered as no action: the host acts for nobody it names"
            );
            return None;
        }
        let Some(shell) = self.shared.link.get().and_then(|link| link.shell()) else {
            tracing::info!(
                %workspace_id,
                action,
                "a Terminal request is answered as no action: this host runs no shells"
            );
            return None;
        };
        let refused = |reason: String| {
            Err(InvocationReceipt {
                invocation,
                status: tilo_actions::remote::ReceiptStatus::Rejected {
                    error: tilo_actions::remote::ActionCallError::Refused { reason },
                },
            })
        };
        let asked = match read(&invoke_arguments(&request.argument)) {
            Ok(asked) => asked,
            Err(reason) => return Some(refused(reason)),
        };
        // The session is a record of this workspace: its keys are here to
        // record what it ran.
        let state = match self.refresh(
            workspace_id,
            Needs::Act {
                app_id: &route.app_id,
                record: &route.document,
            },
        ) {
            Ok(state) => state,
            Err(error) => return Some(refused(error.to_string())),
        };
        let Some(workspace) = state.workspace.clone() else {
            return Some(refused("this host does not hold the workspace".into()));
        };
        let route = DocumentRoute {
            cluster_id: workspace.clone(),
            ..route
        };
        let state = match self.window_default_founded(workspace_id, caller, &route, state) {
            Ok(state) => state,
            Err(outcome) => {
                let reason = match outcome {
                    crate::record_actions::Outcome::Rejected(error) => {
                        return Some(Err(InvocationReceipt {
                            invocation,
                            status: tilo_actions::remote::ReceiptStatus::Rejected { error },
                        }));
                    }
                    crate::record_actions::Outcome::Failed(failure) => failure.message,
                };
                return Some(refused(reason));
            }
        };
        if let Err(receipt) = self.terminal_admitted(workspace_id, caller, request, &route, &state)
        {
            return Some(Err(receipt));
        }
        Some(Ok(Hosted {
            shell,
            workspace,
            route,
            asked,
        }))
    }

    /// `state`, holding the session record at `route` once this founded it when it is a
    /// window's default session that no device wrote yet.
    ///
    /// A fresh Terminal window on a device shows its tab's default session
    /// ([`DocumentId::window_default`](ui::resource::DocumentId::window_default)) and asks
    /// the host for its shell before anything is written to it, as the session lives here.
    /// So the host founds it at that id, with the genesis every device derives for it, so
    /// whatever a device writes to it later merges with this; founded for the caller only
    /// when they may make Terminal records in the workspace ([`Worker::founder`]).
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    fn window_default_founded(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
        state: super::WorkspaceState,
    ) -> Result<super::WorkspaceState, crate::record_actions::Outcome> {
        let Ok(record) = super::reads::document_id(&route.document) else {
            return Ok(state);
        };
        let window_default =
            ui::resource::DocumentId::window_default_of_storage_key(&record.storage_key())
                .is_some_and(|default| default == record);
        if !window_default || state.document(route).is_ok() {
            return Ok(state);
        }
        let who = if self.acts_for_owner(caller) {
            None
        } else {
            self.bounded(caller)
        };
        let founder = self.founder(workspace_id, &route.cluster_id, &route.app_id, who.as_ref())?;
        let at = self.found(
            workspace_id,
            &route.cluster_id,
            &route.app_id,
            crate::apps::Seed::NewAt(record),
            founder,
        )?;
        debug_assert_eq!(&at, route, "a record founded at its id keeps its route");
        tracing::info!(
            %workspace_id,
            record = %route.document,
            acting = ?caller.acting(),
            "founded a window's Terminal session no device wrote yet"
        );
        self.refreshed_for(workspace_id, route, true)
    }

    /// Start `request` when it is a `run` of a Terminal session this host
    /// runs, answering it accepted; `None` for any other request.
    pub(super) fn start_terminal_run(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<InvocationReceipt> {
        let invocation = request.invocation;
        let hosted = self.hosted_session(workspace_id, caller, request, RUN, |arguments| {
            let command = arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            if command.is_empty() || command.len() > MAX_COMMAND_BYTES {
                return Err("a command line of 1 to 4096 bytes".to_owned());
            }
            if command.chars().any(char::is_control) {
                return Err("one command line, without control characters".to_owned());
            }
            Ok(command)
        })?;
        let Hosted {
            shell,
            workspace,
            route,
            asked: command,
        } = match hosted {
            Ok(hosted) => hosted,
            Err(receipt) => return Some(receipt),
        };
        let refused = |reason: String| {
            Some(InvocationReceipt {
                invocation,
                status: tilo_actions::remote::ReceiptStatus::Rejected {
                    error: tilo_actions::remote::ActionCallError::Refused { reason },
                },
            })
        };
        let session_of = caller.agent_session();
        let agent = session_of.is_some();
        let jobs = self.shared.jobs.lock_recover().clone()?;
        let session = shell_session(caller, &route);
        // What a cancel of this invocation, or a Ctrl+C in the session it
        // runs in, stops.
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut log = self.log();
            let Some(entry) = log.entries.get_mut(&invocation) else {
                return refused("the invocation is not this host's".into());
            };
            entry.stop = Some(Arc::clone(&stop));
            drop(log);
        }
        shell.started(&workspace, &session, caller);
        let spawned = Started {
            shell,
            jobs,
            shared: Arc::clone(&self.shared),
            person: self
                .shared
                .link
                .get()
                .and_then(|link| link.person(&workspace, caller)),
            workspace,
            session,
            command,
            agent,
            stop,
            ended: Ended {
                workspace_id,
                invocation,
                route,
                request: request.clone(),
                caller: caller.clone(),
                ran: Ok(ShellRan::default()),
            },
        }
        .spawn();
        if let Err(error) = spawned {
            return refused(format!("the session's shell did not start: {error}"));
        }
        Some(InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }).receipt())
    }

    /// Start `request` when it is an `open-terminal` of a Terminal session this host runs, answering
    /// it accepted: the shell starts on a terminal (or the session it names to come back to
    /// is found) off this thread, and the invocation succeeds with the stream handed to the
    /// caller's device. `None` for any other request.
    pub(super) fn start_terminal_open(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<InvocationReceipt> {
        let invocation = request.invocation;
        let device = caller.device_public_key().copied();
        let hosted = self.hosted_session(workspace_id, caller, request, OPEN, |arguments| {
            let device = device.ok_or_else(|| {
                "a terminal opens for a device, and this caller has none".to_owned()
            })?;
            opening_of(arguments, device)
        })?;
        let Hosted {
            shell,
            workspace,
            route,
            asked: mut opening,
        } = match hosted {
            Ok(hosted) => hosted,
            Err(receipt) => return Some(receipt),
        };
        // Whoever opened it is who the shell runs as, as `run` does for its commands.
        opening.person = self
            .shared
            .link
            .get()
            .and_then(|link| link.person(&workspace, caller));
        opening.workspace = workspace;
        opening.session = shell_session(caller, &route);
        // The opener was let run commands in the session: a program they start there asks
        // them whether it may work in the workspace as their agent, as `run`'s do.
        shell.started(&opening.workspace, &opening.session, caller);
        let jobs = self.shared.jobs.lock_recover().clone()?;
        let (request, caller) = (request.clone(), caller.clone());
        // Each command line the shell runs is recorded as `run`'s are, under this open.
        let record: Recorder = {
            let (jobs, request, caller) = (jobs.clone(), request.clone(), caller.clone());
            Arc::new(move |ran: ShellRan| {
                let ended = Ended {
                    workspace_id,
                    invocation,
                    route: route.clone(),
                    request: request.clone(),
                    caller: caller.clone(),
                    ran: Ok(ran),
                };
                let _ = jobs.send((Instant::now(), Job::Typed(Box::new(ended))));
            })
        };
        let spawned = std::thread::Builder::new()
            .name("workspace-terminal-open".into())
            .spawn(move || {
                let opened = shell.open(&opening, &caller, record);
                let ended = OpenEnded {
                    workspace_id,
                    request,
                    caller,
                    opened,
                };
                let _ = jobs.send((Instant::now(), Job::Opened(Box::new(ended))));
            });
        if let Err(error) = spawned {
            return Some(InvocationReceipt {
                invocation,
                status: tilo_actions::remote::ReceiptStatus::Rejected {
                    error: tilo_actions::remote::ActionCallError::Refused {
                        reason: format!("the session's shell did not start: {error}"),
                    },
                },
            });
        }
        Some(InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }).receipt())
    }

    /// Record `ended`, a command line typed into a terminal an open started, on its session:
    /// its open was answered long before, so nothing else is.
    pub(super) fn typed(&mut self, ended: &Ended) {
        if let Err(error) = self.record_ended(ended) {
            tracing::warn!(error = %error.message, "a command typed on a terminal was not recorded");
        }
    }

    /// Settle the open `ended` with the stream and session it opened.
    pub(super) fn settle_open(&mut self, ended: &OpenEnded) {
        let invocation = ended.request.invocation;
        let update = match &ended.opened {
            Ok(opened) => InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(json!({"stream": opened.stream, "session": opened.session})),
                    resources: Vec::new(),
                },
            },
            Err(message) => InvocationUpdate::Failed {
                invocation,
                error: ActionFailure {
                    code: "terminal_open".into(),
                    message: message.clone(),
                    details: None,
                    retryable: false,
                },
            },
        };
        let phase = match &update {
            InvocationUpdate::Failed { error, .. } => tilo_actions::InteractionPhase::Failed {
                code: Some(error.code.clone()),
                message: error.message.clone(),
            },
            _ => tilo_actions::InteractionPhase::Committed,
        };
        self.voice(
            ended.workspace_id,
            &ended.caller,
            &ended.request,
            (&phase, None),
        );
        self.log().answered(invocation, &update, None);
    }

    /// Whether `caller` may take the Terminal action `request` names on the
    /// session record at `route`, decided as every action the host runs for
    /// someone within bounds is: the caller's access bounded as
    /// [`Caller::invoker`] reads them, the shared admission, and an ask put to
    /// the person on their own devices. The host's own person and their
    /// agents may. A key reaching this workspace alone runs what would be
    /// asked without the ask when its person let the workspace run commands
    /// unattended ([`super::WorkspaceDocumentsLink::runs_unattended`]); what
    /// is refused stays refused. `Err` is the receipt that answers it instead: refused, or
    /// waiting for the person.
    #[allow(
        clippy::result_large_err,
        reason = "the refusal is the receipt the invocation is answered with"
    )]
    fn terminal_admitted(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        route: &DocumentRoute,
        state: &super::WorkspaceState,
    ) -> Result<(), InvocationReceipt> {
        use tilo_actions::remote::{ActionCallError, ReceiptStatus};
        if self.acts_for_owner(caller) {
            return Ok(());
        }
        let invocation = request.invocation;
        let refused = |error| InvocationReceipt {
            invocation,
            status: ReceiptStatus::Rejected { error },
        };
        let turned_away = |why: &str| turned_away(workspace_id, route, request, caller, why);
        let who = self.bounded(caller).ok_or_else(|| {
            turned_away("the host acts for nobody it names");
            refused(ActionCallError::NotPermitted)
        })?;
        let policy = self
            .policy_for(
                workspace_id,
                &who,
                &route.cluster_id,
                Some((&route.app_id, &route.document)),
            )
            .map_err(|error| {
                turned_away(&format!(
                    "the workspace's policy does not admit them: {error}"
                ));
                refused(error)
            })?;
        let permissions = super::bounded::bounded_permissions(&policy, &who)
            .map_err(|error| {
                turned_away(&format!("their access does not reach the record: {error}"));
                refused(error)
            })?
            .reaching_person();
        let mounted = self.mount(route, state).map_err(|error| {
            turned_away(&format!("the session record does not open here: {error}"));
            refused(ActionCallError::UnknownAction)
        })?;
        let descriptor = super::live_actions(&mounted.app)
            .into_iter()
            .find(|descriptor| descriptor.address == request.target.address)
            .ok_or_else(|| {
                turned_away("the session record, as this host reads it, offers no such action");
                refused(ActionCallError::UnknownAction)
            })?;
        let need = descriptor.contract.need();
        let node = tilo_actions::action_node(
            &ui::permissions::policy::TreePath::root(),
            &route.app_id,
            &descriptor.resource_kind,
            &descriptor.address.resource,
            need.field.as_deref(),
            &descriptor.address.action,
        );
        let admitted = ui::permissions::policy::admit(
            ui::permissions::policy::Invoked {
                answer: permissions.answer(&node, need.kind),
                attended: descriptor.contract.is_attended(),
                resource: None,
            },
            permissions.asker(),
        );
        match admitted {
            Err(_) => Err(refused(ActionCallError::NotPermitted)),
            Ok(ui::permissions::policy::Admission::Runs) => Ok(()),
            Ok(ui::permissions::policy::Admission::AsksFirst) => {
                self.terminal_asked(workspace_id, caller, request, route, &descriptor)
            }
        }
    }

    /// An admitted Terminal request whose action asks first: it runs when its person approved
    /// the record it was kept as, or when a key of this workspace alone may run commands
    /// unattended; otherwise its person is asked, and `Err` is the receipt that answers it
    /// meanwhile.
    #[allow(
        clippy::result_large_err,
        reason = "the refusal is the receipt the invocation is answered with"
    )]
    fn terminal_asked(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        route: &DocumentRoute,
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> Result<(), InvocationReceipt> {
        if request.approval().is_some() {
            return Ok(());
        }
        // A key for this workspace alone, whose person let it run
        // commands unattended, runs this one as if they allowed it.
        if self
            .shared
            .link
            .get()
            .is_some_and(|link| link.runs_unattended(workspace_id, caller))
        {
            return Ok(());
        }
        let arguments = super::invoke_arguments(&request.argument);
        // A command is an action of its own. One that carried its resource's other
        // actions would be asked about as it stands: no name is looked up here.
        let carries = super::carried::read(descriptor, &arguments, |_| None);
        let about = self.about(workspace_id, route, descriptor, arguments, carries);
        Err(self.asked(workspace_id, caller, request, &about))
    }

    /// A Terminal `run` or `open-terminal` request starts, or the commands it stops, answered
    /// here: `None` for any other request.
    pub(super) fn terminal(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<InvocationReceipt> {
        let receipt = self
            .start_terminal_run(workspace_id, caller, request)
            .or_else(|| self.start_terminal_open(workspace_id, caller, request))
            .or_else(|| self.stop_terminal_commands(workspace_id, caller, request))?;
        let mut log = self.log();
        log.running.remove(&request.invocation);
        let refused = matches!(
            receipt.status,
            tilo_actions::remote::ReceiptStatus::Rejected { .. }
        );
        if refused {
            log.forget(&request.invocation);
        }
        drop(log);
        if refused {
            self.voice(
                workspace_id,
                caller,
                request,
                (&super::session_voice::refused_phase(), None),
            );
        }
        Some(receipt)
    }

    /// Record `ended` on its session record and settle its invocation with
    /// the command: cancelled, when it was stopped before it ended.
    pub(super) fn ran(&mut self, ended: &Ended) {
        let invocation = ended.invocation;
        let cancelled = ended.ran.as_ref().is_ok_and(|ran| ran.cancelled);
        let update = match self.record_ended(ended) {
            Ok(_) if cancelled => InvocationUpdate::Cancelled {
                invocation,
                message: Some("the command was cancelled".into()),
            },
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => InvocationUpdate::Failed { invocation, error },
        };
        let phase = match &update {
            InvocationUpdate::Failed { error, .. } => tilo_actions::InteractionPhase::Failed {
                code: Some(error.code.clone()),
                message: error.message.clone(),
            },
            _ => tilo_actions::InteractionPhase::Committed,
        };
        self.voice(
            ended.workspace_id,
            &ended.caller,
            &ended.request,
            (&phase, None),
        );
        if let Some(entry) = self.log().entries.get_mut(&invocation) {
            entry.update = update;
            entry.stop = None;
        }
    }

    /// Stop the commands this host has going in the session record `request`
    /// names, as a person's Ctrl+C there does: each ends in the shell that
    /// runs it, records it, and answers that it was cancelled; `None`
    /// for any other request.
    pub(super) fn stop_terminal_commands(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<InvocationReceipt> {
        let invocation = request.invocation;
        if request.target.address.action != STOP {
            return None;
        }
        let route = resource_of(&request.target.provider, "")
            .filter(|route| route.app_id == TERMINAL_APP)?;
        if !self.acts_for_owner(caller) {
            self.bounded(caller)?;
            let refused = |error| {
                Some(InvocationReceipt {
                    invocation,
                    status: tilo_actions::remote::ReceiptStatus::Rejected { error },
                })
            };
            let state = match self.refresh(
                workspace_id,
                Needs::Act {
                    app_id: &route.app_id,
                    record: &route.document,
                },
            ) {
                Ok(state) => state,
                Err(error) => return refused(error),
            };
            let Some(workspace) = state.workspace.clone() else {
                return refused(tilo_actions::remote::ActionCallError::UnknownAction);
            };
            let route = DocumentRoute {
                cluster_id: workspace,
                ..route
            };
            if let Err(receipt) =
                self.terminal_admitted(workspace_id, caller, request, &route, &state)
            {
                return Some(receipt);
            }
        }
        let stopped = {
            let log = self.log();
            let session = request.target.address.resource.clone();
            let mut stopped = 0_u32;
            for entry in log.entries.values() {
                if entry.request.target.address.resource != session {
                    continue;
                }
                if let Some(stop) = &entry.stop
                    && !stop.swap(true, Ordering::SeqCst)
                {
                    stopped = stopped.saturating_add(1);
                }
            }
            drop(log);
            stopped
        };
        let output = ActionOutput {
            value: Some(json!({"stopped": stopped})),
            resources: Vec::new(),
        };
        let update = InvocationUpdate::Succeeded { invocation, output };
        self.log().answered(invocation, &update, None);
        Some(InvocationState::new(invocation, update).receipt())
    }

    #[allow(
        clippy::result_large_err,
        reason = "the failure becomes the invocation's update"
    )]
    fn record_ended(&mut self, ended: &Ended) -> Result<ActionOutput, ActionFailure> {
        let failed = |message: String| ActionFailure {
            code: "terminal_run".into(),
            message,
            details: None,
            retryable: false,
        };
        let ran = ended.ran.clone().map_err(failed)?;
        // The shell keeps the bytes where the workspace's devices fetch them;
        // the command names the reference it answered, as the store that kept them
        // sealed them, which every replica stores as it is.
        let output = (!ran.output.is_empty())
            .then(|| {
                self.shared
                    .link
                    .get()
                    .and_then(|link| link.shell())
                    .and_then(|shell| shell.keep(&ended.route.cluster_id, &ran.output))
            })
            .flatten()
            .map(|reference| ui::tilo_resource::Bytes::kept_as(&ran.output, &reference));
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut finished = Map::new();
        finished.insert("command".into(), json!(ran.command));
        finished.insert("cwd".into(), json!(ran.cwd));
        finished.insert("started_ms".into(), json!(ran.started_ms));
        finished.insert("ended_ms".into(), json!(ran.ended_ms));
        finished.insert("exit".into(), json!(ran.exit));
        if let Some(output) = &output {
            finished.insert("output".into(), json!(output));
        }
        finished.insert("truncated".into(), json!(ran.truncated));
        let (tool, items) = parsed(&ran.command, &ran.output);
        finished.insert("tool".into(), json!(tool));
        finished.insert("undoable".into(), json!(false));
        finished.insert("items".into(), items);
        finished.insert("id".into(), json!(id));
        let state = self
            .refresh(
                ended.workspace_id,
                Needs::Act {
                    app_id: &ended.route.app_id,
                    record: &ended.route.document,
                },
            )
            .map_err(|error| failed(error.to_string()))?;
        let context = caller_context(&ended.caller, &ended.request);
        // The invocation is the caller's, for the person they act for: the host
        // records it on their behalf.
        let invoker = tilo_actions::ActionInvoker::admitted(ended.caller.acting())
            .named(ended.caller.agent_name().map(str::to_owned));
        let holder = {
            self.mount(&ended.route, &state).map_err(failed)?;
            self.holder_of(&ended.route)
        };
        let mounted = self
            .mounted
            .get_mut(&holder)
            .ok_or_else(|| failed("the record did not mount".into()))?;
        mounted
            .app
            .invoke_with(
                Some(&invoker),
                Some(&context),
                &ActionAddress::new(&ended.request.target.address.resource, RECORD_COMMAND),
                &finished,
                true,
            )
            .map_err(|error| failed(format!("the command was not recorded: {error}")))?;
        let taken = take_all(&mounted.app, &holder)
            .map_err(|error| failed(format!("the command was not recorded: {error}")))?;
        self.commit_taken(ended.workspace_id, &ended.route, taken)
            .map_err(|reason| failed(format!("the command was not recorded: {reason}")))?;
        Ok(ActionOutput {
            value: Some(answer(&id, &ran)),
            resources: Vec::new(),
        })
    }
}

/// The shell a caller's commands and terminal run in: an agent's has a shell of its own,
/// wherever its commands are recorded, so two agents in one workspace never share a directory
/// or variables. A person's share the session's shell.
fn shell_session(caller: &Caller, route: &DocumentRoute) -> String {
    caller.agent_session().map_or_else(
        || route.document.clone(),
        |agent_session| format!("run:{}", agent_session.id),
    )
}

/// The terminal `arguments` ask for, opened for `device`: its window (80 by 24 unless named),
/// and the session to come back to.
fn opening_of(arguments: &Map<String, Value>, device: [u8; 32]) -> Result<Opening, String> {
    let side = |name: &str, default: u16| match arguments.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|side| u16::try_from(side).ok())
            .filter(|side| (1..=MAX_SIDE).contains(side))
            .ok_or_else(|| format!("{name} is a whole number from 1 to {MAX_SIDE}")),
    };
    let text = |name: &str, fits: fn(&str) -> bool, what: &str| match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|text| fits(text))
            .map(|text| Some(text.to_owned()))
            .ok_or_else(|| format!("{name} is {what}")),
    };
    Ok(Opening {
        workspace: String::new(),
        session: String::new(),
        device,
        cols: side("cols", DEFAULT_SIZE.0)?,
        rows: side("rows", DEFAULT_SIZE.1)?,
        person: None,
        attach: text(
            "attach",
            |id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "a session's id, 32 hex characters",
        )?,
    })
}

/// What `run` answers: Terminal's answer (the command's id, its exit status, the
/// host's refusal), with the command line, where it ran and the end of what
/// it printed.
fn answer(id: &str, ran: &ShellRan) -> Value {
    let (output, cut) = tail(&ran.output, ANSWERED_OUTPUT);
    let mut answer = Map::new();
    answer.insert("id".into(), json!(id));
    answer.insert("exit".into(), json!(ran.exit));
    answer.insert("command".into(), json!(ran.command));
    answer.insert("cwd".into(), json!(ran.cwd));
    answer.insert("output".into(), json!(output));
    if cut || ran.truncated {
        answer.insert("output_cut".into(), json!(true));
    }
    if let Some(refusal) = &ran.refusal {
        answer.insert(
            "refusal".into(),
            serde_json::to_value(refusal).unwrap_or_default(),
        );
    }
    Value::Object(answer)
}

/// The last `max` bytes of `output` as text, and whether any were left out.
fn tail(output: &[u8], max: usize) -> (String, bool) {
    let start = output.len().saturating_sub(max);
    let text = String::from_utf8_lossy(output.get(start..).unwrap_or_default());
    let text = if start > 0 {
        // Start at a whole line.
        text.split_once('\n')
            .map_or_else(|| text.as_ref(), |(_, rest)| rest)
            .to_owned()
    } else {
        text.into_owned()
    };
    (text, start > 0)
}

/// The tool a command line calls and what Terminal's parsers read in its
/// output (a build's errors, a test's failures), as a device keeps them.
fn parsed(command: &str, output: &[u8]) -> (String, Value) {
    (
        terminal::parse::tool_of(command).to_owned(),
        serde_json::to_value(terminal::parse::items(command, output)).unwrap_or_default(),
    )
}

/// Say why the host turned the Terminal request `request` on `route` away: the wire answers
/// unknown and forbidden alike, so this line is where whoever runs the host reads which.
fn turned_away(
    workspace_id: WorkspaceId,
    route: &DocumentRoute,
    request: &InvokeRequest,
    caller: &Caller,
    why: &str,
) {
    tracing::info!(
        %workspace_id,
        record = %route.document,
        action = %request.target.address.action,
        acting = ?caller.acting(),
        why,
        "a Terminal request was turned away"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_reads_its_window_and_session_and_refuses_what_is_none() {
        let read = |arguments: Value| {
            let Value::Object(arguments) = arguments else {
                panic!("arguments are an object");
            };
            opening_of(&arguments, [7; 32])
        };
        let opening = read(json!({})).unwrap();
        assert_eq!(
            (opening.cols, opening.rows, opening.device),
            (80, 24, [7; 32])
        );
        assert_eq!(opening.attach, None);
        let session = "0123456789abcdef0123456789abcdef";
        let opening = read(json!({"cols": 120, "rows": 40, "attach": session}));
        let opening = opening.unwrap();
        assert_eq!((opening.cols, opening.rows), (120, 40));
        assert_eq!(opening.attach.as_deref(), Some(session));
        assert!(read(json!({"cols": 0})).is_err());
        assert!(read(json!({"rows": 70_000})).is_err());
        assert!(read(json!({"attach": "not-a-session"})).is_err());
    }

    #[test]
    fn an_answer_keeps_the_end_of_a_long_output_from_a_whole_line() {
        let output = format!("{}\nerror: it broke\n", "x".repeat(40));
        let (text, cut) = tail(output.as_bytes(), 20);
        assert_eq!((text.as_str(), cut), ("error: it broke\n", true));
        assert_eq!(tail(b"ok\n", 20), ("ok\n".to_owned(), false));
    }

    #[test]
    fn a_refused_run_answers_the_refusal_typed() {
        let ran = ShellRan {
            command: "cargo build".into(),
            cwd: "/w".into(),
            exit: 126,
            output: b"ush: cargo: no card. Ask the person to add a card".to_vec(),
            refusal: Some(Refusal {
                reason: tilo_actions::remote::server_account::Reason::NoPaymentMethod,
                message: "no card".into(),
                next: Vec::new(),
            }),
            ..ShellRan::default()
        };
        let value = answer("r1", &ran);
        assert_eq!(value["exit"], 126);
        assert_eq!(value["refusal"]["reason"], "no_payment_method");
        assert_eq!(value["cwd"], "/w");
    }

    #[test]
    fn a_command_runs_as_the_agent_that_asked_or_the_person_it_acts_for() {
        use tilo_actions::MemberId;
        let names = vec![
            (MemberId::person("ab12"), "Ada Lovelace".to_owned()),
            (MemberId::agent("Scout"), "Scout".to_owned()),
        ];
        // A person is named as the workspace names them, at its host.
        let person = Caller::local_person("ab12");
        assert_eq!(
            person_of(&person, &names, "https://eu1.tilo.sh"),
            Some(Person {
                name: "Ada Lovelace".into(),
                address: "ada-lovelace@eu1.tilo.sh".into(),
            })
        );
        // An agent is its own member, never the person it acts for.
        let agent = Caller::delegated_agent("Scout", [0xab; 32], [1; 32]);
        assert_eq!(
            person_of(&agent, &names, "eu1.tilo.sh"),
            Some(Person {
                name: "Scout".into(),
                address: "scout@eu1.tilo.sh".into(),
            })
        );
        // An agent the workspace has not called anything goes by the name its session was given.
        let unnamed = Caller::local_agent("reviewer-1").named("Review Bot");
        assert_eq!(
            person_of(&unnamed, &[], "eu1.tilo.sh"),
            Some(Person {
                name: "Review Bot".into(),
                address: "review-bot@eu1.tilo.sh".into(),
            })
        );
        // An agent nothing names signs as an agent, never as the key it is known by.
        let stranger_agent = Caller::local_agent("e3301bbb8b64");
        assert_eq!(
            person_of(&stranger_agent, &[], "team.tilo"),
            Some(Person {
                name: "Tilo agent".into(),
                address: "tilo-agent@team.tilo".into(),
            })
        );
        // A person the workspace has not named is still someone to sign as, and the
        // commits they make carry a name and an address they can read.
        let stranger = Caller::local_person("cd34");
        assert_eq!(
            person_of(&stranger, &[], "team.tilo"),
            Some(Person {
                name: "Tilo member".into(),
                address: "tilo-member@team.tilo".into(),
            })
        );
        // A row the list keeps with no word of a name names nobody either.
        let blank = vec![(MemberId::person("cd34"), "  ".to_owned())];
        assert_eq!(
            person_of(&stranger, &blank, "team.tilo").map(|person| person.name),
            Some("Tilo member".into())
        );
        // Software acting by itself runs as nobody.
        assert_eq!(
            person_of(&Caller::local_app("so.tilo.sheets"), &names, "eu1.tilo.sh"),
            None
        );
    }
}
