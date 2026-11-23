//! The shells of a workspace's Terminal sessions that run on this device.
//!
//! Terminal is a system service beside the workspace, not a window: a workspace's shell lives
//! with the device that keeps the workspace's Linux machine. The device that keeps it answers
//! the app's record actions here, in its own dispatch:
//!
//! - `open-terminal` starts the session's shell on a terminal and hands its stream to the device
//!   that asked, with the id a later open names to come back to it.
//! - `run` runs one command line in the session's shell, records it on the session through the
//!   record's own `record-command`, and answers what it printed.
//! - `stop` stops the session's running commands, as Ctrl+C at its prompt does.
//!
//! A fresh window shows its tab's default session record, which no device wrote yet, and asks
//! for its shell before anything is written to it. This device founds that record at its id,
//! with the genesis every device derives for it, through the same records path the dispatch
//! uses for an app's records; whatever a device writes there later merges with it.
//!
//! The provider decides no grant itself: the device action service, or the embedder's own
//! dispatch, admits each call by the workspace's ordinary grants before it arrives here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationLog,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, Reservation,
    environment_commands::Person, pending, resource_parts,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_agent_session::AppCatalogue;
use tilo_agent_session::shell::{
    Opened, Opening, Recorder, ShellRan, Watching, WorkspaceShell, person_of,
};
use tilo_lock::MutexRecover as _;

use super::Workspaces;
use super::applications::RecordAction;

/// The app whose records these are: Terminal's session records, and the document its machine
/// reads.
pub const TERMINAL_APP: &str = ui::apps::TERMINAL;

/// The provider id this device registers the Terminal shells under: a record's call names
/// `record:<app id>:<record id>`, and the provider registered for the app answers it.
pub const PROVIDER: &str = ui::apps::TERMINAL;

/// A session record's action that runs a command line.
const RUN: &str = "run";
/// A session record's action that opens its shell on a terminal for the caller's device.
const OPEN: &str = "open-terminal";
/// A session record's action that stops the command line it is running.
const STOP: &str = "stop";
/// The resource of the document the workspace's machine reads: the vault items its programs get.
const SECRETS_RESOURCE: &str = "terminal.machine-secrets";
/// The document's action that names a vault item a program in the machine gets.
const ADD_SECRET: &str = "add-secret";
/// The document's action that takes an item back.
const REMOVE_SECRET: &str = "remove-secret";
/// The resource a session's own actions are on.
const SESSION_RESOURCE: &str = "terminal.session";
/// The session record's action that records a command that ended.
const RECORD_COMMAND: &str = "record-command";
/// The window a terminal opens at when the caller names none.
const DEFAULT_SIZE: (u16, u16) = (80, 24);
/// The widest and tallest window a terminal opens at.
const MAX_SIDE: u16 = 1000;
/// The longest command line a session runs, as Terminal takes it.
const MAX_COMMAND_BYTES: usize = 4096;
/// What an answer carries of a command's output at most: its end, where a build's errors and a
/// test summary are.
const ANSWERED_OUTPUT: usize = 32 * 1024;

/// The host-side Terminal: a workspace's shells, and the records of their sessions.
#[derive(Clone)]
pub struct TerminalService {
    copies: Workspaces,
    catalogue: AppCatalogue,
    shells: Arc<dyn WorkspaceShell>,
    runtime: tokio::runtime::Handle,
    log: Arc<Mutex<InvocationLog<InvocationState>>>,
    /// Moves on every change of a command this device runs, so a caller following one looks
    /// again then.
    moved: tokio::sync::watch::Sender<u64>,
    /// The runs going in each session this device serves, by workspace and shell session:
    /// what a `stop` ends.
    stops: Arc<Mutex<HashMap<(String, String), Vec<Arc<AtomicBool>>>>>,
    /// What stops one invocation's run: what a cancel of it ends.
    running: Arc<Mutex<HashMap<InvocationId, Arc<AtomicBool>>>>,
}

impl TerminalService {
    /// The shells of `copies`' workspaces, run on this device by `shells`, answering on
    /// `runtime`.
    #[must_use]
    pub fn new(
        copies: Workspaces,
        shells: Arc<dyn WorkspaceShell>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            copies,
            catalogue: AppCatalogue::native(),
            shells,
            runtime,
            log: Arc::new(Mutex::new(InvocationLog::default())),
            moved: tokio::sync::watch::Sender::new(0),
            stops: Arc::new(Mutex::new(HashMap::new())),
            running: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The workspace and record a `request` of this provider names.
    fn resolved(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(String, String), ActionCallError> {
        let (app, record) =
            resource_parts(&request.target.provider).ok_or(ActionCallError::UnknownAction)?;
        if app != TERMINAL_APP {
            return Err(ActionCallError::UnknownAction);
        }
        let workspace =
            caller
                .workspace()
                .map(str::to_owned)
                .ok_or(ActionCallError::InvalidRequest {
                    reason: "a Terminal session's shell runs in one workspace".into(),
                })?;
        Ok((workspace, record.to_owned()))
    }

    /// Start a session's shell on a terminal for `caller`'s device: founding the session record
    /// when nothing wrote it, then handing the stream over. The answer names the stream and the
    /// session to come back to.
    async fn opening(
        self: &Arc<Self>,
        caller: &Caller,
        workspace: &str,
        record: &str,
        mut opening: Opening,
    ) -> Result<ActionOutput, ActionCallError> {
        self.machine(workspace).await?;
        self.found(workspace, record).await?;
        opening.workspace = workspace.to_owned();
        opening.session = shell_session(caller, record);
        opening.person = self.person(workspace, caller).await;
        self.shells
            .started(&opening.workspace, &opening.session, caller);
        let shells = Arc::clone(&self.shells);
        let recorder: Recorder = {
            let service = Arc::clone(self);
            let caller = caller.clone();
            let workspace = workspace.to_owned();
            let record = record.to_owned();
            Arc::new(move |ran: ShellRan| {
                service.record(&workspace, &record, &caller, ran);
            })
        };
        let Opened { stream, session } = shells
            .open(&opening, caller, recorder)
            .map_err(|reason| ActionCallError::Refused { reason })?;
        Ok(ActionOutput {
            value: Some(json!({ "stream": stream, "session": session })),
            resources: Vec::new(),
        })
    }

    /// Run `command`, the command line `request` asks for, in the session's shell off this
    /// thread; the caller follows the invocation to what it printed.
    fn run(
        self: &Arc<Self>,
        caller: &Caller,
        workspace: String,
        record: String,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let command = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.get("command").and_then(Value::as_str),
            _ => None,
        }
        .unwrap_or_default()
        .trim()
        .to_owned();
        if command.is_empty() || command.len() > MAX_COMMAND_BYTES {
            return Pending::ready(Err(ActionCallError::InvalidRequest {
                reason: "a command line of 1 to 4096 bytes".into(),
            }));
        }
        if command.chars().any(char::is_control) {
            return Pending::ready(Err(ActionCallError::InvalidRequest {
                reason: "one command line, without control characters".into(),
            }));
        }
        let invocation = request.invocation;
        let caller = caller.clone();
        match self.log.lock_recover().reserve(&caller, request) {
            Ok(Reservation::Reserved) => {}
            // The same invocation, asked again: it is already running or ended here.
            Ok(Reservation::Admitted) => {
                let answered = self.log.lock_recover().get(&caller, &invocation).cloned();
                return match answered {
                    Some(state) => Pending::ready(Ok(state.receipt())),
                    None => Pending::ready(Err(ActionCallError::UnknownInvocation)),
                };
            }
            Err(refused) => return Pending::ready(Err(refused)),
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.running
            .lock_recover()
            .insert(invocation, Arc::clone(&stop));
        {
            let session = shell_session(&caller, &record);
            self.stops
                .lock_recover()
                .entry((workspace.clone(), session))
                .or_default()
                .push(Arc::clone(&stop));
        }
        let started = InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
        self.log
            .lock_recover()
            .complete(&invocation, started.clone());
        let service = Arc::clone(self);
        self.runtime.spawn(async move {
            let update = service
                .run_and_record(&caller, &workspace, &record, &command, &stop, invocation)
                .await;
            service.note(invocation, update);
        });
        Pending::ready(Ok(started.receipt()))
    }

    /// Run `command` in the session's shell and record it on its session record.
    async fn run_and_record(
        &self,
        caller: &Caller,
        workspace: &str,
        record: &str,
        command: &str,
        stop: &Arc<AtomicBool>,
        invocation: InvocationId,
    ) -> InvocationUpdate {
        if let Err(refusal) = self.machine(workspace).await {
            return InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "unavailable".into(),
                    message: refusal.said(),
                    details: None,
                    retryable: false,
                },
            };
        }
        if let Err(refusal) = self.found(workspace, record).await {
            return InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "unavailable".into(),
                    message: refusal.said(),
                    details: None,
                    retryable: false,
                },
            };
        }
        let shells = Arc::clone(&self.shells);
        let session = shell_session(caller, record);
        let person = self.person(workspace, caller).await;
        let agent = caller.agent_session().is_some();
        let stopping = Arc::clone(stop);
        let stop_requested = move || stopping.load(Ordering::SeqCst);
        let ran = {
            let (workspace, command, by) =
                (workspace.to_owned(), command.to_owned(), caller.clone());
            self.runtime
                .spawn_blocking(move || {
                    shells.run(
                        &workspace,
                        &session,
                        &command,
                        &by,
                        agent,
                        person.as_ref(),
                        Watching {
                            seen: None,
                            stop: Some(&stop_requested),
                        },
                    )
                })
                .await
        };
        let ran = match ran {
            Ok(Ok(ran)) => ran,
            Ok(Err(reason)) => return failed(invocation, reason),
            Err(error) => return failed(invocation, error.to_string()),
        };
        let id = self.record(workspace, record, caller, ran.clone());
        if ran.cancelled {
            return InvocationUpdate::Cancelled {
                invocation,
                message: Some("the command was cancelled".into()),
            };
        }
        InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(answer(&id, &ran)),
                resources: Vec::new(),
            },
        }
    }

    /// Stop this device's commands running in the session `record` names, as Ctrl+C at its
    /// prompt does.
    fn stop(&self, caller: &Caller, workspace: &str, record: &str) -> ActionOutput {
        let session = shell_session(caller, record);
        let stops: Vec<Arc<AtomicBool>> = self
            .stops
            .lock_recover()
            .get(&(workspace.to_owned(), session))
            .cloned()
            .unwrap_or_default();
        let stopped = stops
            .iter()
            .filter(|stop| !stop.swap(true, Ordering::SeqCst))
            .count();
        ActionOutput {
            value: Some(json!({ "stopped": stopped })),
            resources: Vec::new(),
        }
    }

    /// Whether this computer is `workspace`'s machine: the workspace is the person's own, its
    /// only people the person this device acts for. A workspace other people are members of
    /// runs its programs in a VM on a host, never unconfined on someone's computer.
    async fn machine(&self, workspace: &str) -> Result<(), ActionCallError> {
        let summary =
            self.copies
                .summary(workspace)
                .await
                .map_err(|error| ActionCallError::Unavailable {
                    reason: format!("the workspace's people could not be read: {error}"),
                })?;
        if summary.people() > 1 {
            return Err(ActionCallError::Refused {
                reason: "this computer runs only its own person's workspace's programs".into(),
            });
        }
        Ok(())
    }

    /// Run the app's `action` on the workspace's machine secrets document, founded when no
    /// window made it, and publish what it changed as the governor's.
    async fn declared(
        &self,
        caller: &Caller,
        workspace: &str,
        record: &str,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<ActionOutput, ActionCallError> {
        self.found(workspace, record).await?;
        let invoker = tilo_actions::ActionInvoker::admitted(caller.acting());
        let asked = RecordAction {
            app: TERMINAL_APP,
            record,
            resource: SECRETS_RESOURCE,
            action,
            arguments,
        };
        self.copies
            .apply_app_action(
                workspace,
                self.copies.governor(),
                &self.catalogue,
                &invoker,
                asked,
            )
            .await
            .map_err(|error| ActionCallError::Refused {
                reason: error.to_string(),
            })
    }

    /// Found `record`, a record of Terminal's in `workspace`, when nothing wrote it yet: a fresh
    /// window's tab default session, or the document the workspace's machine reads, with the
    /// genesis every device derives for it.
    async fn found(&self, workspace: &str, record: &str) -> Result<(), ActionCallError> {
        let held = self
            .copies
            .resource_document(workspace, TERMINAL_APP, record)
            .await
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("the session's record could not be read: {error}"),
            })?;
        if held.is_some() {
            return Ok(());
        }
        let declared = record == tilo_environment::machine_secrets::DOCUMENT;
        if !declared && !is_window_default(record) {
            return Err(ActionCallError::UnknownAction);
        }
        if !self.copies.governs_workspace(workspace).await {
            return Err(ActionCallError::Refused {
                reason: "the workspace's host founds its sessions' records".into(),
            });
        }
        let created = self
            .copies
            .prepare_application(
                workspace,
                self.copies.governor(),
                &self.catalogue,
                TERMINAL_APP,
                record,
            )
            .await
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("the session's record could not be founded: {error}"),
            })?;
        if created {
            tracing::info!(%workspace, %record, "founded a Terminal record no device wrote yet");
        }
        Ok(())
    }

    /// Record `ran`, a command line the session ran, on its session record through the app's
    /// `record-command` as the command's invoker. Answers the command's id.
    fn record(&self, workspace: &str, record: &str, caller: &Caller, ran: ShellRan) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut finished = Map::new();
        finished.insert("command".into(), json!(ran.command));
        finished.insert("cwd".into(), json!(ran.cwd));
        finished.insert("started_ms".into(), json!(ran.started_ms));
        finished.insert("ended_ms".into(), json!(ran.ended_ms));
        finished.insert("exit".into(), json!(ran.exit));
        if !ran.output.is_empty() {
            finished.insert(
                "output".into(),
                json!(tilo_resource::Bytes::of(&ran.output)),
            );
        }
        finished.insert("truncated".into(), json!(ran.truncated));
        finished.insert("tool".into(), json!(""));
        finished.insert("undoable".into(), json!(false));
        finished.insert("items".into(), json!([]));
        finished.insert("id".into(), json!(id));
        let copies = self.copies.clone();
        let catalogue = self.catalogue.clone();
        let governor = copies.governor().clone();
        let (workspace, record) = (workspace.to_owned(), record.to_owned());
        let invoker = tilo_actions::ActionInvoker::admitted(caller.acting());
        self.runtime.spawn(async move {
            let asked = RecordAction {
                app: TERMINAL_APP,
                record: &record,
                resource: SESSION_RESOURCE,
                action: RECORD_COMMAND,
                arguments: finished,
            };
            if let Err(error) = copies
                .apply_app_action(&workspace, &governor, &catalogue, &invoker, asked)
                .await
            {
                tracing::warn!(%workspace, %record, %error, "a Terminal command was not recorded");
            }
        });
        id
    }

    /// Who `caller` is in `workspace`, as the workspace names them: the name and address its
    /// programs record as the author of what they make.
    async fn person(&self, workspace: &str, caller: &Caller) -> Option<Person> {
        let summary = self.copies.summary(workspace).await.ok()?;
        let names: Vec<(tilo_actions::MemberId, String)> = summary
            .members
            .into_iter()
            .map(|member| (member.id.clone(), member.name))
            .collect();
        person_of(caller, &names, &format!("{}.tilo", summary.name))
    }

    /// Note `update`, the end of an invocation this device ran, and wake whoever follows it.
    fn note(&self, invocation: InvocationId, update: InvocationUpdate) {
        self.log
            .lock_recover()
            .complete(&invocation, InvocationState::new(invocation, update));
        self.moved
            .send_modify(|moves| *moves = moves.wrapping_add(1));
        let mut stops = self.stops.lock_recover();
        stops.retain(|_, stops| {
            stops.retain(|stop| !stop.load(Ordering::SeqCst));
            !stops.is_empty()
        });
    }
}

/// Why a command did not run, as the invocation's failure.
fn failed(invocation: InvocationId, reason: String) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: "terminal_run".into(),
            message: format!("the session's shell did not run it: {reason}"),
            details: None,
            retryable: true,
        },
    }
}

/// The shell a caller's commands and terminal run in: an agent's has a shell of its own,
/// wherever its commands are recorded, so two agents in one workspace never share a directory
/// or variables. A person's share the session's shell.
fn shell_session(caller: &Caller, record: &str) -> String {
    caller.agent_session().map_or_else(
        || record.to_owned(),
        |session| format!("run:{}", session.id),
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

/// Whether `record` is a fresh window's tab default: what a Terminal on a new tab shows before
/// anything wrote it.
fn is_window_default(record: &str) -> bool {
    collab_types::document::DocumentId::parse(record).is_some_and(|id| {
        collab_types::document::DocumentId::window_default_of_storage_key(&id.storage_key())
            .is_some_and(|default| default == id)
    })
}

/// What `run` answers: Terminal's answer (the command's id and exit status), with the command
/// line, where it ran and the end of what it printed.
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

impl ActionProvider for TerminalService {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER || resource_parts(provider).is_some_and(|(app, _)| app == TERMINAL_APP)
    }

    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        // A session record's actions are its app's, and the catalogue lists them where the app
        // runs. A call naming the record reaches this device's shells without a listing.
        Pending::ready(Ok(CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        let (workspace, record) = match self.resolved(caller, &request) {
            Ok(resolved) => resolved,
            Err(refused) => return Pending::ready(Err(refused)),
        };
        let service = Arc::new(self.clone());
        let invocation = request.invocation;
        let update = |output: ActionOutput, invocation: InvocationId| InvocationUpdate::Succeeded {
            invocation,
            output,
        };
        match request.target.address.action.as_str() {
            OPEN => {
                let Some(device) = caller.device_public_key().copied() else {
                    return Pending::ready(Err(ActionCallError::InvalidRequest {
                        reason: "a terminal opens for a device, and this caller has none".into(),
                    }));
                };
                let arguments = match &request.argument {
                    InvokeArgument::Json(arguments) => arguments.clone(),
                    _ => Map::new(),
                };
                let opening = match opening_of(&arguments, device) {
                    Ok(opening) => opening,
                    Err(reason) => {
                        return Pending::ready(Err(ActionCallError::InvalidRequest { reason }));
                    }
                };
                let caller = caller.clone();
                let (completer, answer) = pending();
                self.runtime.spawn(async move {
                    let answered =
                        match service.opening(&caller, &workspace, &record, opening).await {
                            Ok(output) => {
                                let state =
                                    InvocationState::new(invocation, update(output, invocation));
                                service
                                    .log
                                    .lock_recover()
                                    .complete(&invocation, state.clone());
                                Ok(state.receipt())
                            }
                            Err(refusal) => Err(refusal),
                        };
                    completer.complete(answered);
                });
                answer
            }
            RUN => service.run(caller, workspace, record, &request),
            STOP => {
                let output = self.stop(caller, &workspace, &record);
                Pending::ready(Ok(
                    self.note_and_receipt(invocation, update(output, invocation))
                ))
            }
            // What the workspace's machine reads: the items its programs get, changed by the
            // app's own actions on the record, founded when no window made it.
            ADD_SECRET | REMOVE_SECRET if request.target.address.resource == SECRETS_RESOURCE => {
                let arguments = match &request.argument {
                    InvokeArgument::Json(arguments) => arguments.clone(),
                    _ => Map::new(),
                };
                let action = request.target.address.action.clone();
                let caller = caller.clone();
                let (completer, answer) = pending();
                self.runtime.spawn(async move {
                    let answered = match service
                        .declared(&caller, &workspace, &record, &action, arguments)
                        .await
                    {
                        Ok(output) => {
                            let state =
                                InvocationState::new(invocation, update(output, invocation));
                            service
                                .log
                                .lock_recover()
                                .complete(&invocation, state.clone());
                            Ok(state.receipt())
                        }
                        Err(refusal) => Err(refusal),
                    };
                    completer.complete(answered);
                });
                answer
            }
            _ => Pending::ready(Err(ActionCallError::UnknownAction)),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.log.lock_recover().get(caller, id).cloned()
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.log.lock_recover().get(caller, id)?;
        let mut moved = self.moved.subscribe();
        let (changed, change) = pending();
        self.runtime.spawn(async move {
            // Bounded, as the action service's contract asks: a change missed costs a look.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), moved.changed()).await;
            changed.complete(());
        });
        Some(change)
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        match self.running.lock_recover().get(id) {
            Some(stop) => {
                stop.store(true, Ordering::SeqCst);
                Pending::ready(Ok(()))
            }
            None => Pending::ready(Err(ActionCallError::UnknownInvocation)),
        }
    }
}

impl TerminalService {
    /// Record `update` and answer its receipt.
    fn note_and_receipt(
        &self,
        invocation: InvocationId,
        update: InvocationUpdate,
    ) -> InvocationReceipt {
        let state = InvocationState::new(invocation, update);
        self.log.lock_recover().complete(&invocation, state.clone());
        state.receipt()
    }
}
