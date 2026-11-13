//! A member device that takes its workspaces' work: their shell commands
//! ([`tilo_actions::remote::environment_runs`]) and their Browser
//! ([`tilo_actions::remote::browser`]), in their VMs on the hosted-VM service.
//!
//! [`EnvironmentRuns`] admits a [`RUN`](environment_runs::RUN) from the workspace's
//! host, runs the command line in the session's shell over the member's own copy of the
//! workspace's files ([`WorkspaceShells`]), streams what it prints, and answers how it
//! went. The host can stop a run it started, which ends the command as Ctrl+C does
//! ([`CommandRan::cancelled`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use tilo_actions::remote::browser::{self, OpenBrowser};
use tilo_actions::remote::environment_runs::{self, CommandRan, RunCommand, RunOutput};
use tilo_actions::remote::linux_apps::{self, OpenApp};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationLedger,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, Reservation,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};

use crate::environment_browsers::{AppRefused, BrowserHost};
use crate::workspace_shell::{Payer, ShellRun, WorkspaceShells};

const PROFILE_STORE: &str = ".tilo/browser";

const REMEMBERED: usize = 1024;

const CHANGE_WAIT: Duration = Duration::from_secs(1);

pub struct EnvironmentRuns(Arc<Inner>);

struct Inner {
    shells: WorkspaceShells,
    browsers: Option<BrowserHost>,
    operator: [u8; 32],
    ledger: Mutex<InvocationLedger<Run>>,
    moved: Condvar,
    /// How many times a run moved, so a waiter tells a change from a wake.
    moves: Mutex<u64>,
}

#[derive(Clone)]
struct Run {
    state: Arc<Mutex<InvocationState>>,
    stop: Arc<AtomicBool>,
}

impl Run {
    fn state(&self) -> InvocationState {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl EnvironmentRuns {
    #[must_use]
    pub fn new(shells: WorkspaceShells, operator: [u8; 32]) -> Self {
        let size = shells.hosted_size();
        let browsers = shells
            .hosted_socket()
            .map(|socket| BrowserHost::new(socket, size));
        Self(Arc::new(Inner {
            shells,
            browsers,
            operator,
            ledger: Mutex::new(InvocationLedger::with_capacity(REMEMBERED)),
            moved: Condvar::new(),
            moves: Mutex::new(0),
        }))
    }
}

fn not_served() -> ActionHostError {
    ActionHostError::Unavailable {
        reason: "this member runs its workspaces' shell commands and Browser".into(),
    }
}

fn argument(request: &InvokeRequest) -> Result<RunCommand, ActionHostError> {
    let InvokeArgument::Json(map) = &request.argument else {
        return Err(ActionHostError::InvalidRequest {
            reason: "a run takes its command as JSON".into(),
        });
    };
    serde_json::from_value(serde_json::Value::Object(map.clone())).map_err(|error| {
        ActionHostError::InvalidRequest {
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

    fn start(self: &Arc<Self>, run: Run, workspace: String, asked: RunCommand) {
        let inner = Arc::clone(self);
        let invocation = run.state().invocation();
        std::thread::spawn(move || {
            let sequence = std::sync::atomic::AtomicU64::new(0);
            let seen = |printed: &RunOutput| {
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
                size: legacy_vm::hosted::size::VmSize::parse(&asked.size).unwrap_or_default(),
            };
            let ran = inner.shells.run_for(
                &workspace,
                &asked.session,
                &asked.command,
                asked.agent,
                Some(&payer),
                Some(&stopping),
                Some(&seen),
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

impl ActionHost for EnvironmentRuns {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Err(not_served()))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        Pending::ready(self.admit(caller, &request))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let ledger = self.0.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        ledger.get(caller, id).map(Run::state)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.observe(caller, id)?;
        let (changed, change) = tilo_actions::remote::pending();
        let inner = Arc::clone(&self.0);
        std::thread::spawn(move || {
            inner.wait_for_a_move();
            changed.complete(());
        });
        Some(change)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        let run = self
            .0
            .ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(caller, id)
            .cloned();
        let Some(run) = run else {
            return Pending::ready(Err(ActionHostError::UnknownInvocation));
        };
        if run.state().update.is_terminal() {
            return Pending::ready(Err(ActionHostError::NotCancellable {
                reason: "the run has ended".into(),
            }));
        }
        // The command ends in the session's shell, as it does on a person's
        // Ctrl+C; the run answers with what it printed until then.
        run.stop.store(true, Ordering::SeqCst);
        Pending::ready(Ok(()))
    }
}

impl EnvironmentRuns {
    fn admit(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let target = &request.target;
        if target.provider != environment_runs::PROVIDER
            || target.address.resource != environment_runs::RESOURCE
        {
            return Err(not_served());
        }
        let action = target.address.action.as_str();
        let in_vm = [
            browser::OPEN,
            browser::STOP,
            linux_apps::OPEN,
            linux_apps::STOP,
        ]
        .contains(&action);
        let browsers = self.0.browsers.as_ref().filter(|_| in_vm);
        if in_vm && browsers.is_none() {
            return Err(ActionHostError::Unavailable {
                reason: "this member runs no workspace VMs, so no Browser".into(),
            });
        }
        // Only the workspace's host asks: a device of this member's own
        // operator. Anyone else could name an account that is not theirs.
        if caller.user_public_key() != Some(&self.0.operator) {
            return Err(ActionHostError::NotPermitted);
        }
        let workspace = caller
            .workspace()
            .ok_or(ActionHostError::NotPermitted)?
            .to_owned();
        let asked = match action {
            environment_runs::RUN => Asked::Run(argument(request)?),
            browser::OPEN => Asked::Browser(Some(open_argument(request, "Browser")?)),
            browser::STOP => Asked::Browser(None),
            linux_apps::OPEN => Asked::App(Some(open_argument(request, "a Linux app")?)),
            linux_apps::STOP => Asked::App(None),
            _ => return Err(not_served()),
        };
        let invocation = request.invocation;
        let mut ledger = self.0.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        match ledger.reserve(caller, request)? {
            Reservation::Admitted => ledger
                .get(caller, &invocation)
                .map(|run| run.state().receipt())
                .ok_or(ActionHostError::UnknownInvocation),
            Reservation::Reserved => {
                let run = Run {
                    state: Arc::new(Mutex::new(InvocationState::new(
                        invocation,
                        InvocationUpdate::Accepted { invocation },
                    ))),
                    stop: Arc::new(AtomicBool::new(false)),
                };
                ledger.complete(&invocation, run.clone());
                drop(ledger);
                let receipt = run.state().receipt();
                match asked {
                    Asked::Run(asked) => self.0.start(run, workspace, asked),
                    Asked::Browser(asked) => {
                        let browsers = browsers.cloned();
                        self.0.start_browser(run, workspace, asked, browsers);
                    }
                    Asked::App(asked) => {
                        let browsers = browsers.cloned();
                        self.0.start_app(run, workspace, asked, browsers);
                    }
                }
                Ok(receipt)
            }
        }
    }
}

enum Asked {
    Run(RunCommand),
    Browser(Option<OpenBrowser>),
    App(Option<OpenApp>),
}

fn open_argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionHostError> {
    let InvokeArgument::Json(map) = &request.argument else {
        return Err(ActionHostError::InvalidRequest {
            reason: format!("opening {what} takes its workspace as JSON"),
        });
    };
    serde_json::from_value(serde_json::Value::Object(map.clone())).map_err(|error| {
        ActionHostError::InvalidRequest {
            reason: format!("not {what} to open: {error}"),
        }
    })
}

#[cfg(test)]
#[path = "environment_runs_tests.rs"]
mod tests;
