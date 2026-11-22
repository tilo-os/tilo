//! A member device that takes its workspaces' work: their shell commands
//! ([`tilo_actions::remote::environment_commands`]) and their Browser
//! ([`tilo_actions::remote::browser`]), in their VMs on the hosted-VM service.
//!
//! [`EnvironmentCommands`] admits a [`RUN`](environment_commands::RUN) from the workspace's
//! host, runs the command line in the session's shell over the member's own copy of the
//! workspace's files ([`WorkspaceShells`]), streams what it prints, and answers how it
//! went. The host can stop a run it started, which ends the command as Ctrl+C does
//! ([`CommandRan::cancelled`]). It also branches a workspace's machine from another's and
//! deletes one ([`tilo_actions::remote::environment_machines`]), on the hosted-VM service's own
//! socket. And it starts a program in a workspace's machine that the workspace's server serves
//! from afar, and holds it for that server ([`tilo_actions::remote::environment_programs`],
//! [`crate::environment_programs`]). And it opens a Terminal session's shell on a terminal
//! ([`tilo_actions::remote::environment_terminals`]), whose stream it hands the server that
//! asked, to take on [`crate::environment_terminals::ALPN`] ([`EnvironmentCommands::serve_terminal`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use tilo_actions::remote::browser::{self, OpenBrowser};
use tilo_actions::remote::environment_commands::{self, CommandOutput, CommandRan, CommandRequest};
use tilo_actions::remote::environment_machines::{self, BranchMachine, MachineBranched};
use tilo_actions::remote::environment_programs::{self, ServeProgram, TellProgram};
use tilo_actions::remote::environment_terminals::{self, OpenTerminal, TerminalOpened};
use tilo_actions::remote::linux_apps::{self, OpenApp};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationLog,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, Reservation,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};

use crate::environment_browsers::{AppRefused, BrowserHost};
use crate::environment_programs::{Held, LEFT_AFTER, Starts};
use crate::environment_terminals::{Ended, EnvironmentTerminals, RanQueue, TerminalAsk};
use crate::workspace_shell::{Payer, RunWith, ShellRun, WorkspaceShells};

const PROFILE_STORE: &str = ".tilo/browser";

const REMEMBERED: usize = 1024;

const CHANGE_WAIT: Duration = Duration::from_secs(1);

/// How long the hosted-VM service is given to branch or delete a machine: a source asleep is
/// woken for its copy.
const MACHINE_PATIENCE: Duration = Duration::from_secs(300);

/// How often a branch waiting for room to wake its source asks again.
const MACHINE_RETRY: Duration = Duration::from_secs(2);

pub struct EnvironmentCommands(Arc<Inner>);

struct Inner {
    shells: Arc<WorkspaceShells>,
    /// The sessions' shells on terminals: the same shells.
    terminals: EnvironmentTerminals,
    browsers: Option<BrowserHost>,
    operator: [u8; 32],
    log: Mutex<InvocationLog<Run>>,
    /// The served programs held for their servers, each under the invocation that started
    /// it: found here also once the log has forgotten that invocation.
    programs: Mutex<HashMap<InvocationId, Program>>,
    /// What starts a served program, when it is not the machine the shells name.
    starts: Option<Box<Starts>>,
    /// How long a served program is kept when its server no longer asks after it.
    left_after: Duration,
    moved: Condvar,
    /// How many times a run moved, so a waiter tells a change from a wake.
    moves: Mutex<u64>,
}

#[derive(Clone)]
struct Run {
    state: Arc<Mutex<InvocationState>>,
    stop: Arc<AtomicBool>,
}

/// A served program this member holds, with the invocation that started it.
type Program = (Run, Arc<Held>);

impl Run {
    fn state(&self) -> InvocationState {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl EnvironmentCommands {
    #[must_use]
    pub fn new(shells: WorkspaceShells, operator: [u8; 32]) -> Self {
        let size = shells.hosted_size();
        let browsers = shells
            .hosted_socket()
            .map(|socket| BrowserHost::new(socket, size));
        let shells = Arc::new(shells);
        Self(Arc::new(Inner {
            terminals: EnvironmentTerminals::new(Arc::clone(&shells)),
            shells,
            browsers,
            operator,
            log: Mutex::new(InvocationLog::with_capacity(REMEMBERED)),
            programs: Mutex::new(HashMap::new()),
            starts: None,
            left_after: LEFT_AFTER,
            moved: Condvar::new(),
            moves: Mutex::new(0),
        }))
    }

    /// These, starting a served program by `starts` instead of in the machine the shells
    /// name: for a member whose machines are stood in.
    #[must_use]
    pub fn starting_programs_by(mut self, starts: Box<Starts>) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.0) {
            inner.starts = Some(starts);
        }
        self
    }

    /// These, ending a served program whose server has not asked after it for `after`.
    #[must_use]
    pub fn leaving_programs_after(mut self, after: Duration) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.0) {
            inner.left_after = after;
        }
        self
    }
}

impl EnvironmentCommands {
    /// Carry the terminal stream this member handed `device` (the device the endpoint that
    /// dialled asks as) over `from` and `to`.
    ///
    /// # Errors
    ///
    /// The stream named none handed to `device`, or its hello could not be read.
    pub fn serve_terminal(
        &self,
        device: [u8; 32],
        from: impl std::io::Read + Send + 'static,
        to: impl std::io::Write,
    ) -> std::io::Result<Ended> {
        self.0.terminals.serve_handed(device, from, to)
    }
}

fn not_served() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "this member runs its workspaces' shell commands and Browser".into(),
    }
}

fn argument(request: &InvokeRequest) -> Result<CommandRequest, ActionCallError> {
    let InvokeArgument::Json(map) = &request.argument else {
        return Err(ActionCallError::InvalidRequest {
            reason: "a run takes its command as JSON".into(),
        });
    };
    serde_json::from_value(serde_json::Value::Object(map.clone())).map_err(|error| {
        ActionCallError::InvalidRequest {
            reason: format!("not a command to run: {error}"),
        }
    })
}

impl Inner {
    fn moved(&self) {
        let mut moves = self.moves.lock().unwrap_or_else(PoisonError::into_inner);
        *moves = moves.wrapping_add(1);
        drop(moves);
        self.moved.notify_all();
    }

    /// Wait until a run moves, or [`CHANGE_WAIT`] passes.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the count stays locked from reading it to waiting on it, so no move falls between"
    )]
    fn wait_for_a_move(&self) {
        let moves = self.moves.lock().unwrap_or_else(PoisonError::into_inner);
        let seen = *moves;
        drop(
            self.moved
                .wait_timeout_while(moves, CHANGE_WAIT, |moves| *moves == seen),
        );
    }

    fn set(&self, run: &Run, update: InvocationUpdate) {
        let invocation = run.state().invocation();
        *run.state.lock().unwrap_or_else(PoisonError::into_inner) =
            InvocationState::new(invocation, update);
        self.moved();
    }

    fn start_browser(
        self: &Arc<Self>,
        run: Run,
        workspace: String,
        asked: Option<OpenBrowser>,
        browsers: Option<BrowserHost>,
    ) {
        let inner = Arc::clone(self);
        let invocation = run.state().invocation();
        std::thread::spawn(move || {
            let outcome = match (asked, browsers) {
                (Some(mut asked), Some(browsers)) => {
                    asked.workspace.clone_from(&workspace);
                    let store = inner.shells.files(&workspace).join(PROFILE_STORE);
                    browsers.open(&asked, store).map(Some)
                }
                (None, Some(browsers)) => browsers.stop(&workspace).map(|()| None),
                _ => Err("this member runs no workspace VMs".to_owned()),
            };
            let update = match outcome {
                Ok(opened) => InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput {
                        value: opened.and_then(|opened| serde_json::to_value(opened).ok()),
                        resources: Vec::new(),
                    },
                },
                Err(message) => InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: "unavailable".into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                },
            };
            inner.set(&run, update);
        });
    }

    /// Answer `run` by starting (or ending) `workspace`'s Linux apps in its
    /// VM on this host's hosted-VM service. A refusal by the account's plan
    /// is answered as one, carrying the plans' typed refusal, so whoever
    /// opened the app reads what would let it through.
    fn start_app(
        self: &Arc<Self>,
        run: Run,
        workspace: String,
        asked: Option<OpenApp>,
        browsers: Option<BrowserHost>,
    ) {
        let inner = Arc::clone(self);
        let invocation = run.state().invocation();
        std::thread::spawn(move || {
            let outcome = match (asked, browsers) {
                (Some(mut asked), Some(browsers)) => {
                    asked.workspace.clone_from(&workspace);
                    browsers.open_app(&asked).map(Some)
                }
                (None, Some(browsers)) => browsers
                    .stop_apps(&workspace)
                    .map(|()| None)
                    .map_err(AppRefused::Error),
                _ => Err(AppRefused::Error(
                    "this member runs no workspace VMs".to_owned(),
                )),
            };
            let update = match outcome {
                Ok(opened) => InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput {
                        value: opened.and_then(|opened| serde_json::to_value(opened).ok()),
                        resources: Vec::new(),
                    },
                },
                Err(AppRefused::Refused(refusal)) => InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: linux_apps::REFUSED.into(),
                        message: refusal.message.clone(),
                        details: serde_json::to_value(&refusal).ok(),
                        retryable: false,
                    },
                },
                Err(AppRefused::Error(message)) => InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: "unavailable".into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                },
            };
            inner.set(&run, update);
        });
    }

    /// Open `asked` on a terminal, or come back to the session it names, and hand its stream
    /// to `device`, the server's device that asked.
    fn start_terminal(&self, run: &Run, workspace: &str, asked: &OpenTerminal, device: [u8; 32]) {
        let invocation = run.state().invocation();
        let payer = Payer {
            account: asked.account.clone(),
            size: linux_vm::hosted::size::VmSize::parse(&asked.size).unwrap_or_default(),
        };
        // The session's runs are kept for the server, which records them, to follow.
        let runs = RanQueue::new();
        let opened = self
            .terminals
            .open_for(
                device,
                &TerminalAsk {
                    workspace,
                    session: &asked.session,
                    payer: Some(&payer),
                    person: asked.person.as_ref(),
                    cols: asked.cols,
                    rows: asked.rows,
                    ran: Some(runs.to()),
                    attach: asked.attach.as_deref(),
                    secrets: &asked.secrets,
                },
            )
            .and_then(|opened| {
                runs.of(&opened.session);
                let runs = opened
                    .started
                    .then(|| self.terminals.hand_runs(device, runs))
                    .transpose()
                    .map_err(|error| error.to_string())?;
                Ok((opened, runs))
            });
        let update = match opened {
            Ok((opened, runs)) => InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: serde_json::to_value(TerminalOpened {
                        stream: opened.stream,
                        session: opened.session,
                        runs,
                    })
                    .ok(),
                    resources: Vec::new(),
                },
            },
            Err(message) => InvocationUpdate::Failed {
                invocation,
                error: ActionFailure {
                    code: "unavailable".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        };
        self.set(run, update);
    }

    fn start(self: &Arc<Self>, run: Run, workspace: String, asked: CommandRequest) {
        let inner = Arc::clone(self);
        let invocation = run.state().invocation();
        std::thread::spawn(move || {
            let sequence = std::sync::atomic::AtomicU64::new(0);
            let seen = |printed: &CommandOutput| {
                let value = serde_json::to_value(printed).unwrap_or_default();
                let sequence = sequence.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                inner.set(
                    &run,
                    InvocationUpdate::Item {
                        invocation,
                        sequence,
                        value,
                    },
                );
            };
            let stop = Arc::clone(&run.stop);
            let stopping = || stop.load(Ordering::SeqCst);
            let payer = Payer {
                account: asked.account.clone(),
                size: linux_vm::hosted::size::VmSize::parse(&asked.size).unwrap_or_default(),
            };
            let ran = inner.shells.run_for(
                &workspace,
                &asked.session,
                &asked.command,
                asked.agent,
                RunWith {
                    payer: Some(&payer),
                    person: asked.person.as_ref(),
                    secrets: &asked.secrets,
                    stopping: Some(&stopping),
                    seen: Some(&seen),
                },
            );
            let update = match ran {
                Ok(ran) => InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput {
                        value: serde_json::to_value(ran_of(ran)).ok(),
                        resources: Vec::new(),
                    },
                },
                Err(error) => InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: "unavailable".into(),
                        message: format!("the workspace's shell could not start: {error}"),
                        details: None,
                        retryable: true,
                    },
                },
            };
            inner.set(&run, update);
        });
    }
}

impl Inner {
    /// The served program `id`, when `caller` is the server it is held for: a device of this
    /// member's operator, asking as the program's own workspace.
    fn program(&self, caller: &Caller, id: &InvocationId) -> Option<Program> {
        if caller.user_public_key() != Some(&self.operator) {
            return None;
        }
        let workspace = caller.workspace()?;
        let programs = self.programs.lock().unwrap_or_else(PoisonError::into_inner);
        let found = programs
            .get(id)
            .filter(|(_, held)| held.of(workspace))
            .cloned();
        drop(programs);
        found
    }

    /// Hold `program` under `invocation`, letting go of the programs that are over.
    fn hold(&self, invocation: InvocationId, program: Program) {
        let mut programs = self.programs.lock().unwrap_or_else(PoisonError::into_inner);
        programs.retain(|_, (over, _)| !over.state().update.is_terminal());
        programs.insert(invocation, program);
    }

    /// Answer `going` by starting what `asked` names in `workspace`'s machine and holding it
    /// for the workspace's server until it is over: each update shows what the program sent
    /// that the server has not taken, and the answer says how it ended.
    fn start_program(self: &Arc<Self>, going: Run, workspace: String, asked: ServeProgram) {
        let inner = Arc::clone(self);
        let invocation = going.state().invocation();
        std::thread::spawn(move || {
            let started = Held::start(
                &inner.shells,
                inner.starts.as_deref(),
                &workspace,
                asked,
                inner.left_after,
            );
            let update = match started {
                Ok(held) => {
                    let held = Arc::new(held);
                    inner.hold(invocation, (going.clone(), Arc::clone(&held)));
                    let sequence = std::sync::atomic::AtomicU64::new(0);
                    let ended = held.follow(&going.stop, &|frames| {
                        let sequence = sequence.fetch_add(1, Ordering::Relaxed);
                        let value = serde_json::to_value(frames).unwrap_or_default();
                        inner.set(
                            &going,
                            InvocationUpdate::Item {
                                invocation,
                                sequence,
                                value,
                            },
                        );
                    });
                    InvocationUpdate::Succeeded {
                        invocation,
                        output: ActionOutput {
                            value: serde_json::to_value(ended).ok(),
                            resources: Vec::new(),
                        },
                    }
                }
                Err(message) => InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: "unavailable".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            };
            inner.set(&going, update);
        });
    }
}

impl Inner {
    /// Answer `run` by making `workspace`'s machine a branch of the one `asked` names, or by
    /// deleting it, on this host's hosted-VM service.
    fn start_machine(self: &Arc<Self>, run: Run, workspace: String, asked: Option<BranchMachine>) {
        let inner = Arc::clone(self);
        let invocation = run.state().invocation();
        std::thread::spawn(move || {
            let outcome = match (inner.shells.hosted_socket(), asked) {
                (None, _) => Err("this member runs no workspace VMs".to_owned()),
                (Some(socket), Some(asked)) => branch_machine(&socket, &workspace, &asked)
                    .map(|branched| serde_json::to_value(MachineBranched { branched }).ok()),
                (Some(socket), None) => delete_machine(&socket, &workspace).map(|()| None),
            };
            let update = match outcome {
                Ok(value) => InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput {
                        value,
                        resources: Vec::new(),
                    },
                },
                Err(message) => InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: "unavailable".into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                },
            };
            inner.set(&run, update);
        });
    }
}

/// Make `workspace`'s machine a branch of `asked.from`'s on the hosted-VM service at `socket`:
/// `true` once its disk is there, `false` when the source has no machine, so nothing was made. A
/// source asleep on a host with no room to wake it is asked for again until there is.
fn branch_machine(
    socket: &std::path::Path,
    workspace: &str,
    asked: &BranchMachine,
) -> Result<bool, String> {
    use linux_vm::hosted::control::{ENVIRONMENT, Reply, Request, VmRequest};
    let size = linux_vm::hosted::size::VmSize::parse(&asked.size)
        .ok_or_else(|| format!("{:?} is not a machine's size", asked.size))?;
    let request = Request::Branch {
        vm: VmRequest {
            workspace: workspace.to_owned(),
            environment: ENVIRONMENT.to_owned(),
            account: asked.account.clone(),
            size,
        },
        from: asked.from.clone(),
    };
    let until = std::time::Instant::now().checked_add(MACHINE_PATIENCE);
    loop {
        match ask_service(socket, &request)? {
            Reply::Done => return Ok(true),
            Reply::NoVm { .. } => return Ok(false),
            Reply::Busy(_) if until.is_some_and(|until| std::time::Instant::now() < until) => {
                std::thread::sleep(MACHINE_RETRY);
            }
            Reply::Busy(words) | Reply::Error(words) => return Err(words),
            Reply::Refused(refusal) => return Err(refusal.message),
            other => return Err(format!("the hosted-VM service answered {other:?}")),
        }
    }
}

/// Delete `workspace`'s machine on the hosted-VM service at `socket`: done too when it had none.
fn delete_machine(socket: &std::path::Path, workspace: &str) -> Result<(), String> {
    use linux_vm::hosted::control::{ENVIRONMENT, Reply, Request};
    let request = Request::Delete {
        workspace: workspace.to_owned(),
        environment: ENVIRONMENT.to_owned(),
    };
    match ask_service(socket, &request)? {
        Reply::Done => Ok(()),
        Reply::Busy(words) | Reply::Error(words) => Err(words),
        other => Err(format!("the hosted-VM service answered {other:?}")),
    }
}

fn ask_service(
    socket: &std::path::Path,
    request: &linux_vm::hosted::control::Request,
) -> Result<linux_vm::hosted::control::Reply, String> {
    linux_vm::hosted::control::ask(socket, request, MACHINE_PATIENCE)
        .map_err(|error| format!("the hosted-VM service at {}: {error}", socket.display()))?
        .ok_or_else(|| {
            format!(
                "no hosted-VM service answers at {}, so no machine was changed",
                socket.display()
            )
        })
}

fn ran_of(ran: ShellRun) -> CommandRan {
    CommandRan {
        command: ran.command,
        cwd: ran.cwd,
        exit: ran.exit,
        output: String::from_utf8_lossy(&ran.output).into_owned(),
        truncated: ran.truncated,
        cancelled: ran.cancelled,
        started_ms: ran.started_ms,
        ended_ms: ran.ended_ms,
        refusal: ran.refusal,
    }
}

impl ActionProvider for EnvironmentCommands {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(not_served()))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(self.admit(caller, &request))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        // A served program's server asks after it by looking at it: that is what keeps it.
        let held = self.0.program(caller, id);
        if let Some((_, held)) = &held {
            held.asked_after();
        }
        let log = self.0.log.lock().unwrap_or_else(PoisonError::into_inner);
        let known = log.get(caller, id).map(Run::state);
        drop(log);
        known.or_else(|| held.map(|(going, _)| going.state()))
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.observe(caller, id)?;
        // A served program's server waiting for its next change asks after it until the wait
        // is answered, however long the program stays quiet.
        let waiting = self.0.program(caller, id).map(|(_, held)| held.waited_on());
        let (changed, change) = tilo_actions::remote::pending();
        let inner = Arc::clone(&self.0);
        std::thread::spawn(move || {
            inner.wait_for_a_move();
            drop(waiting);
            changed.complete(());
        });
        Some(change)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        let run = self
            .0
            .log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(caller, id)
            .cloned()
            .or_else(|| self.0.program(caller, id).map(|(going, _)| going));
        let Some(run) = run else {
            return Pending::ready(Err(ActionCallError::UnknownInvocation));
        };
        if run.state().update.is_terminal() {
            return Pending::ready(Err(ActionCallError::NotCancellable {
                reason: "the run has ended".into(),
            }));
        }
        // The command ends in the session's shell, as it does on a person's
        // Ctrl+C; the run answers with what it printed until then.
        run.stop.store(true, Ordering::SeqCst);
        Pending::ready(Ok(()))
    }
}

impl EnvironmentCommands {
    fn admit(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let target = &request.target;
        if target.provider != environment_commands::PROVIDER
            || target.address.resource != environment_commands::RESOURCE
        {
            return Err(not_served());
        }
        let action = target.address.action.as_str();
        let in_vm = [
            browser::OPEN,
            browser::STOP,
            linux_apps::OPEN,
            linux_apps::STOP,
            environment_machines::BRANCH,
            environment_machines::DELETE,
        ]
        .contains(&action);
        let browsers = self.0.browsers.as_ref().filter(|_| in_vm);
        if in_vm && browsers.is_none() {
            return Err(ActionCallError::Unavailable {
                reason: "this member runs no workspace VMs, so no Browser".into(),
            });
        }
        // Only the workspace's host asks: a device of this member's own
        // operator. Anyone else could name an account that is not theirs.
        if caller.user_public_key() != Some(&self.0.operator) {
            return Err(ActionCallError::NotPermitted);
        }
        let workspace = caller
            .workspace()
            .ok_or(ActionCallError::NotPermitted)?
            .to_owned();
        if action == environment_programs::TELL {
            return self.tell(caller, request);
        }
        let asked = match action {
            environment_commands::RUN => Asked::Command(argument(request)?),
            environment_terminals::OPEN => {
                let device = *caller
                    .device_public_key()
                    .ok_or(ActionCallError::NotPermitted)?;
                Asked::Terminal(program_argument(request, "a terminal to open")?, device)
            }
            environment_programs::SERVE => {
                Asked::Program(program_argument(request, "a program to serve")?)
            }
            browser::OPEN => Asked::Browser(Some(open_argument(request, "Browser")?)),
            browser::STOP => Asked::Browser(None),
            linux_apps::OPEN => Asked::App(Some(open_argument(request, "a Linux app")?)),
            linux_apps::STOP => Asked::App(None),
            environment_machines::BRANCH => Asked::Machine(Some(branch_argument(request)?)),
            environment_machines::DELETE => Asked::Machine(None),
            _ => return Err(not_served()),
        };
        let invocation = request.invocation;
        let mut log = self.0.log.lock().unwrap_or_else(PoisonError::into_inner);
        match log.reserve(caller, request)? {
            Reservation::Admitted => log
                .get(caller, &invocation)
                .map(|run| run.state().receipt())
                .ok_or(ActionCallError::UnknownInvocation),
            Reservation::Reserved => {
                let run = Run {
                    state: Arc::new(Mutex::new(InvocationState::new(
                        invocation,
                        InvocationUpdate::Accepted { invocation },
                    ))),
                    stop: Arc::new(AtomicBool::new(false)),
                };
                log.complete(&invocation, run.clone());
                drop(log);
                let receipt = run.state().receipt();
                match asked {
                    Asked::Command(asked) => self.0.start(run, workspace, asked),
                    Asked::Browser(asked) => {
                        let browsers = browsers.cloned();
                        self.0.start_browser(run, workspace, asked, browsers);
                    }
                    Asked::App(asked) => {
                        let browsers = browsers.cloned();
                        self.0.start_app(run, workspace, asked, browsers);
                    }
                    Asked::Machine(asked) => self.0.start_machine(run, workspace, asked),
                    Asked::Program(asked) => self.0.start_program(run, workspace, asked),
                    Asked::Terminal(asked, device) => {
                        self.0.start_terminal(&run, &workspace, &asked, device);
                    }
                }
                Ok(receipt)
            }
        }
    }

    /// Hand a served program what its server tells it, at once: nothing of it is kept in the
    /// log, since a frame is handed on once by its number however often it is sent.
    fn tell(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let told: TellProgram = program_argument(request, "what a served program is told")?;
        let (_, held) = self
            .0
            .program(caller, &told.program)
            .ok_or(ActionCallError::UnknownInvocation)?;
        held.tell(&told)?;
        let invocation = request.invocation;
        Ok(InvocationState::new(
            invocation,
            InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: None,
                    resources: Vec::new(),
                },
            },
        )
        .receipt())
    }
}

enum Asked {
    Command(CommandRequest),
    Browser(Option<OpenBrowser>),
    App(Option<OpenApp>),
    /// A branch of the workspace's machine from another's, or its deletion.
    Machine(Option<BranchMachine>),
    /// A program to start in the workspace's machine and hold for the workspace's server.
    Program(ServeProgram),
    /// A session's shell to open on a terminal, for the server's device that asked.
    Terminal(OpenTerminal, [u8; 32]),
}

fn branch_argument(request: &InvokeRequest) -> Result<BranchMachine, ActionCallError> {
    let InvokeArgument::Json(map) = &request.argument else {
        return Err(ActionCallError::InvalidRequest {
            reason: "a machine's branch names its source as JSON".into(),
        });
    };
    serde_json::from_value(serde_json::Value::Object(map.clone())).map_err(|error| {
        ActionCallError::InvalidRequest {
            reason: format!("not a machine to branch: {error}"),
        }
    })
}

fn program_argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionCallError> {
    let InvokeArgument::Json(map) = &request.argument else {
        return Err(ActionCallError::InvalidRequest {
            reason: format!("{what} is asked for as JSON"),
        });
    };
    serde_json::from_value(serde_json::Value::Object(map.clone())).map_err(|error| {
        ActionCallError::InvalidRequest {
            reason: format!("not {what}: {error}"),
        }
    })
}

fn open_argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionCallError> {
    let InvokeArgument::Json(map) = &request.argument else {
        return Err(ActionCallError::InvalidRequest {
            reason: format!("opening {what} takes its workspace as JSON"),
        });
    };
    serde_json::from_value(serde_json::Value::Object(map.clone())).map_err(|error| {
        ActionCallError::InvalidRequest {
            reason: format!("not {what} to open: {error}"),
        }
    })
}

#[cfg(test)]
#[path = "environment_commands_tests.rs"]
mod tests;
