//! Terminal sessions the host runs: a hosted workspace's shell lives with the
//! workspace, beside the host's VMs.
//!
//! On a device, a Terminal session record's `run` queues the command line
//! for the process running the session, which types it at its shell and
//! records the run it became (`record-run`). A workspace the host serves has
//! no such process on any device: the host is where its sessions run. So a
//! person's device (an agent on one of their computers among them) asking
//! the host for a session's `run` has it run here, by the host's
//! [`WorkspaceShell`], in that session's shell (an agent's run in a shell of its
//! own, which keeps its directory between commands), and the host records the
//! run on the session record through the record's own `record-run`, under
//! the invocation's context: History keeps it as the agent's run, with its
//! command, directory, exit status and output.
//!
//! A command may run for minutes (a build in the workspace's VM), so it runs
//! on a thread of its own: the invocation is answered accepted at once, and
//! succeeds with the run once it ended and was recorded. Its answer is what
//! Terminal answers a `run` with, plus what an agent needs without a second
//! call: the command line, the directory it ran in and what it printed.
//!
//! While it runs, the invocation says what the command printed so far
//! ([`Watching::seen`]), so an agent following a long build is not silent;
//! and it can be stopped: a person's Ctrl+C in the session, or an agent's
//! cancel of its invocation, ends the command in the host's shell
//! ([`Watching::stop`]), records the run like any other, and settles the
//! invocation as cancelled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use serde_json::{Map, Value, json};
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::environment_runs::RunOutput;
use tilo_actions::remote::host_account::Refusal;
use tilo_actions::remote::{Caller, InvocationReceipt, InvocationState, InvokeRequest};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;

use super::{Job, Needs, Shared, Worker, caller_context, invoke_arguments, take_all};
use crate::participant::actions::record_of;

/// Terminal's app id: its session records are what the host runs.
pub const TERMINAL_APP: &str = "so.tilo.terminal";
/// The session record's action that runs a command line.
const RUN: &str = "run";
/// The session record's action that stops the command line it is running.
const STOP: &str = "stop";
/// The session record's action that records a run that ended.
const RECORD_RUN: &str = "record-run";
/// The longest command line a session runs, as Terminal takes it.
const MAX_COMMAND_BYTES: usize = 4096;
/// What an answer carries of a run's output at most: its end, where a
/// build's errors and a test run's summary are.
const ANSWERED_OUTPUT: usize = 32 * 1024;

/// What the caller of a run sees of it while it runs, and how it stops it.
#[derive(Clone, Copy, Default)]
pub struct Watching<'a> {
    /// What a run printed so far, each time that grew.
    pub seen: Option<&'a (dyn Fn(&RunOutput) + Sync)>,
    /// Whether the caller asked the run to stop.
    pub stop: Option<&'a (dyn Fn() -> bool + Sync)>,
}

/// Where a host's workspaces run their shells.
pub trait WorkspaceShell: Send + Sync {
    /// Run `command` in `workspace`'s Terminal session `session`; `agent`
    /// when an agent asked, whose output is rendered for it and whose
    /// refused program is not waited for. `watching` shows the caller what
    /// the command prints while it runs, and stops it when the caller asks.
    ///
    /// # Errors
    ///
    /// The session's shell could not be started.
    fn run(
        &self,
        workspace: &str,
        session: &str,
        command: &str,
        agent: bool,
        watching: Watching<'_>,
    ) -> Result<ShellRan, String>;

    /// Keep `output`, a run's output, where the workspace's devices fetch
    /// its content: its content hash, or `None` when this host keeps none.
    fn keep(&self, workspace: &str, output: &[u8]) -> Option<String> {
        let _ = (workspace, output);
        None
    }
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
    /// cancel of the run it asked for.
    pub cancelled: bool,
    pub started_ms: i64,
    pub ended_ms: i64,
    /// Why the host would not run a program the command needed, typed.
    pub refusal: Option<Refusal>,
}

/// A run that ended, for the worker to record.
pub(super) struct Ended {
    pub(super) workspace_id: u64,
    pub(super) invocation: InvocationId,
    pub(super) route: DocumentRoute,
    pub(super) request: InvokeRequest,
    pub(super) ran: Result<ShellRan, String>,
}

/// One run asked of the host's shell, ready to start on a thread of its own.
struct Started {
    shell: Arc<dyn WorkspaceShell>,
    jobs: mpsc::Sender<(Instant, Job)>,
    shared: Arc<Shared>,
    workspace: String,
    session: String,
    command: String,
    agent: bool,
    /// What a cancel of the run, or a Ctrl+C in its session, sets.
    stop: Arc<AtomicBool>,
    ended: Ended,
}

impl Started {
    /// Run the command, showing the invocation what it prints as it prints
    /// it, and hand the run to the worker to record once it ended.
    fn spawn(self) -> std::io::Result<()> {
        let Self {
            shell,
            jobs,
            shared,
            workspace,
            session,
            command,
            agent,
            stop,
            ended,
        } = self;
        let invocation = ended.invocation;
        // What the command prints as it prints it, where the invocation's
        // answer is read: an agent following a long build sees the newest
        // output rather than silence.
        let printing = move |printed: &RunOutput| {
            let mut ledger = shared.ledger.lock_recover();
            if let Some(entry) = ledger.entries.get_mut(&invocation) {
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
            drop(ledger);
        };
        std::thread::Builder::new()
            .name("workspace-shell".into())
            .spawn(move || {
                let stopping = || stop.load(Ordering::SeqCst);
                let watching = Watching {
                    seen: Some(&printing),
                    stop: Some(&stopping),
                };
                let ran = shell.run(&workspace, &session, &command, agent, watching);
                let _ = jobs.send((Instant::now(), Job::Ran(Box::new(Ended { ran, ..ended }))));
            })
            .map(drop)
    }
}

impl Worker {
    /// Start `request` when it is a `run` of a Terminal session this host
    /// runs, answering it accepted; `None` for any other request.
    pub(super) fn start_terminal_run(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<InvocationReceipt> {
        let invocation = request.invocation;
        if request.target.address.action != RUN || !self.acts_for_owner(caller) {
            return None;
        }
        let route =
            record_of(&request.target.provider, "").filter(|route| route.app_id == TERMINAL_APP)?;
        let shell = self.shared.link.get()?.shell()?;
        let refused = |reason: String| {
            Some(InvocationReceipt {
                invocation,
                status: tilo_actions::remote::ReceiptStatus::Rejected {
                    error: tilo_actions::remote::ActionHostError::Refused { reason },
                },
            })
        };
        let arguments = invoke_arguments(&request.argument);
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if command.is_empty() || command.len() > MAX_COMMAND_BYTES {
            return refused("a command line of 1 to 4096 bytes".into());
        }
        if command.chars().any(char::is_control) {
            return refused("one command line, without control characters".into());
        }
        // The session is a record of this workspace: its keys are here to
        // record the run once it ended.
        let state = match self.refresh(
            workspace_id,
            Needs::Act {
                app_id: &route.app_id,
                record: &route.record,
            },
        ) {
            Ok(state) => state,
            Err(error) => return refused(error.to_string()),
        };
        let Some(workspace) = state.workspace else {
            return refused("this host does not hold the workspace".into());
        };
        let route = DocumentRoute {
            cluster_id: workspace.clone(),
            ..route
        };
        let run = request
            .context
            .as_ref()
            .and_then(|context| context.group.as_ref())
            .filter(|group| group.kind == "agent-run" && !group.id.is_empty());
        let agent = run.is_some();
        let jobs = self.shared.jobs.lock_recover().clone()?;
        // An agent's run has a shell of its own, wherever its runs are
        // recorded: two agents in one workspace never share a directory
        // or variables. A person's commands share the session's shell.
        let session = run.map_or_else(|| route.record.clone(), |run| format!("run:{}", run.id));
        // What a cancel of this invocation, or a Ctrl+C in the session it
        // runs in, stops.
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut ledger = self.ledger();
            let Some(entry) = ledger.entries.get_mut(&invocation) else {
                return refused("the run is not this host's".into());
            };
            entry.stop = Some(Arc::clone(&stop));
            drop(ledger);
        }
        let spawned = Started {
            shell,
            jobs,
            shared: Arc::clone(&self.shared),
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
                ran: Ok(ShellRan::default()),
            },
        }
        .spawn();
        if let Err(error) = spawned {
            return refused(format!("the session's shell did not start: {error}"));
        }
        Some(InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }).receipt())
    }

    /// Record `ended` on its session record and settle its invocation with
    /// the run: cancelled, when it was stopped before it ended.
    pub(super) fn ran(&mut self, ended: &Ended) {
        let invocation = ended.invocation;
        let cancelled = ended.ran.as_ref().is_ok_and(|ran| ran.cancelled);
        let update = match self.record_ended(ended) {
            Ok(_) if cancelled => InvocationUpdate::Cancelled {
                invocation,
                message: Some("the run was cancelled".into()),
            },
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => InvocationUpdate::Failed { invocation, error },
        };
        if let Some(entry) = self.ledger().entries.get_mut(&invocation) {
            entry.update = update;
            entry.stop = None;
        }
    }

    /// Stop the runs this host has going in the session record `request`
    /// names, as a person's Ctrl+C there does: each ends in the shell that
    /// runs it, records its run, and answers that it was cancelled; `None`
    /// for any other request.
    pub(super) fn stop_terminal_runs(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<InvocationReceipt> {
        let invocation = request.invocation;
        if request.target.address.action != STOP || !self.acts_for_owner(caller) {
            return None;
        }
        record_of(&request.target.provider, "").filter(|route| route.app_id == TERMINAL_APP)?;
        let stopped = {
            let ledger = self.ledger();
            let session = request.target.address.resource.clone();
            let mut stopped = 0_u32;
            for entry in ledger.entries.values() {
                if entry.request.target.address.resource != session {
                    continue;
                }
                if let Some(stop) = &entry.stop
                    && !stop.swap(true, Ordering::SeqCst)
                {
                    stopped = stopped.saturating_add(1);
                }
            }
            drop(ledger);
            stopped
        };
        let output = ActionOutput {
            value: Some(json!({"stopped": stopped})),
            resources: Vec::new(),
        };
        let update = InvocationUpdate::Succeeded { invocation, output };
        if let Some(entry) = self.ledger().entries.get_mut(&invocation) {
            entry.update = update.clone();
        }
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
        let output = (!ran.output.is_empty())
            .then(|| {
                self.shared
                    .link
                    .get()
                    .and_then(|link| link.shell())
                    .and_then(|shell| shell.keep(&ended.route.cluster_id, &ran.output))
            })
            .flatten();
        let run = uuid::Uuid::new_v4().simple().to_string();
        let mut finished = Map::new();
        finished.insert("command".into(), json!(ran.command));
        finished.insert("cwd".into(), json!(ran.cwd));
        finished.insert("started_ms".into(), json!(ran.started_ms));
        finished.insert("ended_ms".into(), json!(ran.ended_ms));
        finished.insert("exit".into(), json!(ran.exit));
        if let Some(hash) = &output {
            finished.insert("output".into(), json!(hash));
        }
        finished.insert(
            "output_len".into(),
            json!(if output.is_some() {
                ran.output.len()
            } else {
                0
            }),
        );
        finished.insert("truncated".into(), json!(ran.truncated));
        let (tool, items) = parsed(&ran.command, &ran.output);
        finished.insert("tool".into(), json!(tool));
        finished.insert("undoable".into(), json!(false));
        finished.insert("items".into(), items);
        finished.insert("run".into(), json!(run));
        let state = self
            .refresh(
                ended.workspace_id,
                Needs::Act {
                    app_id: &ended.route.app_id,
                    record: &ended.route.record,
                },
            )
            .map_err(|error| failed(error.to_string()))?;
        let context = caller_context(&ended.request);
        let mounted = self.mount(&ended.route, &state).map_err(failed)?;
        mounted
            .app
            .invoke_in(
                context.as_ref(),
                None,
                &ActionAddress::new(&ended.request.target.address.resource, RECORD_RUN),
                &finished,
                true,
            )
            .map_err(|error| failed(format!("the run was not recorded: {error}")))?;
        let taken = take_all(&mounted.app, &ended.route)
            .map_err(|error| failed(format!("the run was not recorded: {error}")))?;
        self.commit_taken(ended.workspace_id, &ended.route, taken)
            .map_err(|reason| failed(format!("the run was not recorded: {reason}")))?;
        Ok(ActionOutput {
            value: Some(answer(&run, &ran)),
            resources: Vec::new(),
        })
    }
}

/// What `run` answers: Terminal's answer (the run, its exit status, the
/// host's refusal), with the command line, where it ran and the end of what
/// it printed.
fn answer(run: &str, ran: &ShellRan) -> Value {
    let (output, cut) = tail(&ran.output, ANSWERED_OUTPUT);
    let mut answer = Map::new();
    answer.insert("run".into(), json!(run));
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

/// The tool a command line runs and what Terminal's parsers read in its
/// output (a build's errors, a test run's failures), as a device records
/// them.
#[cfg(target_os = "linux")]
fn parsed(command: &str, output: &[u8]) -> (String, Value) {
    (
        terminal::parse::tool_of(command).to_owned(),
        serde_json::to_value(terminal::parse::items(command, output)).unwrap_or_default(),
    )
}

/// Without Terminal linked, a run is recorded without its parsed items.
#[cfg(not(target_os = "linux"))]
fn parsed(_command: &str, _output: &[u8]) -> (String, Value) {
    (String::new(), json!([]))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                reason: tilo_actions::remote::host_account::Reason::NoPaymentMethod,
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
}
