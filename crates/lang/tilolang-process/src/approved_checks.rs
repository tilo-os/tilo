//! Checks the project's host approved, run for an app's `run-approved-check` (`engine.check`).
//!
//! The check is the device's work for the invocation the dispatch is serving: the app calls the
//! device's [`approved_checks::RUN`] through the one dispatch as that invocation's work, naming
//! only the check's id. The device reads which app, record and resource it is for from the
//! invocation its dispatch delivered to this app's connection, rechecks it while the check runs,
//! and its approved-projects setting picks the commands, the sources and the bounds.
//!
//! The action's code ends once it started the check, and the invocation stays open until the
//! check ends, answered then with the receipt of what the device observed. How far the device
//! said the check got is the invocation's progress, and the app reads the same through
//! `engine.job(job)`, the device's record of it; `engine.cancel(job)`, or the invocation's own
//! cancel, stops it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use collaboration::approved_checks::{self, Receipt};
use collaboration::{CollabRequest, CollaborationManager};
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};

/// Why no check starts where the instance runs on its device alone: no dispatch carries it.
pub const ON_THIS_DEVICE_ALONE: &str = "No project's approved checks run here: this app runs on \
     this device alone, with no workspace host to run them";

/// Why no check starts outside an invocation the workspace dispatched: the device runs a check
/// only as the work of one.
pub const NOT_DISPATCHED: &str = "An approved check runs for an action the workspace dispatched: \
     run it from the app's actions, or ask an agent to";

/// Why no check starts from an action other than the app's `run-approved-check`.
pub const NOT_THE_CHECK_ACTION: &str =
    "Only the app's own run-approved-check action runs an approved check";

/// How many checks that ended are kept for the app to read before the oldest is forgotten.
const ENDED_KEPT: usize = 16;

/// How long one ask of how a running check stands waits for it to move.
const FOLLOW_MS: u32 = 10_000;

/// The invocation the broker served whose code asks for a check: its id and the action it names.
pub struct Served<'a> {
    pub invocation: InvocationId,
    pub action: &'a str,
}

/// How a check ended.
#[derive(Clone, Debug)]
enum End {
    /// The device ran it and observed this.
    Observed(Box<Receipt>),
    /// It did not run, or the device could not observe it: why.
    Failed(String),
    /// It was stopped.
    Cancelled,
}

/// How far the device said a check got, and how it ended.
#[derive(Default)]
struct Heard {
    /// How many reports were heard: the sequence of the newest.
    reports: u64,
    latest: Option<(String, Option<u64>, Option<u64>)>,
    end: Option<End>,
}

/// One check this instance started.
struct Job {
    /// The invocation the check is the work of.
    invocation: InvocationId,
    /// The call to the device that runs it.
    call: InvocationId,
    heard: Heard,
    /// The broker's delivery the check is the work of, and what the action's run answered, once
    /// that run ended: the invocation is answered with it when the check ends.
    answers: Option<(u64, ActionOutput)>,
    /// How many reports the broker was told as the invocation's progress.
    told: u64,
    /// Whether its end was answered, or answers nobody.
    settled: bool,
    /// The device's record of it, which every window of the app reads: `None` where the device
    /// keeps no jobs.
    recorded: Option<crate::engines::Recorded>,
}

/// The checks one instance started, by the job id the app holds.
pub struct Checks {
    /// The dispatch the calls go out through; `None` for an instance with no broker.
    manager: Option<Rc<RefCell<CollaborationManager>>>,
    jobs: RefCell<BTreeMap<String, Job>>,
    next: Cell<u64>,
}

impl Checks {
    /// The checks of an instance whose calls go out through `manager` (`None` with no broker).
    #[must_use]
    pub fn new(manager: Option<Rc<RefCell<CollaborationManager>>>) -> Self {
        Self {
            manager,
            jobs: RefCell::default(),
            next: Cell::new(1),
        }
    }

    /// Send `message` to the dispatch.
    fn send(&self, message: RemoteActionMessage) -> Result<(), String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| ON_THIS_DEVICE_ALONE.to_owned())?;
        manager
            .borrow_mut()
            .try_send(CollabRequest::RemoteAction {
                message,
                press: None,
            })
            .map_err(|error| error.to_string())
    }

    /// Start the check `check_id` for the invocation `served` the broker is serving: the job id
    /// the app follows it by.
    ///
    /// # Errors
    ///
    /// Why it does not start: no broker, no served invocation, an action other than the app's
    /// `run-approved-check`, the call not reaching the dispatch, or the device not keeping its
    /// record (`record`).
    pub fn start(
        &self,
        served: Option<Served<'_>>,
        check_id: &str,
        record: impl FnOnce() -> Option<Result<crate::engines::Recorded, String>>,
    ) -> Result<String, String> {
        if self.manager.is_none() {
            return Err(ON_THIS_DEVICE_ALONE.to_owned());
        }
        let served = served.ok_or_else(|| NOT_DISPATCHED.to_owned())?;
        if served.action != approved_checks::ACTION_ID {
            return Err(NOT_THE_CHECK_ACTION.to_owned());
        }
        let call = InvocationId::new();
        let mut arguments = serde_json::Map::new();
        arguments.insert(approved_checks::ARGUMENT.to_owned(), check_id.into());
        self.send(RemoteActionMessage::Invoke(InvokeRequest {
            invocation: call,
            target: RemoteActionTarget {
                provider: approved_checks::PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    approved_checks::RESOURCE,
                    approved_checks::RUN,
                ),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: Some(served.invocation),
            wait_ms: None,
            appearance: None,
            signed: None,
        }))
        .map_err(|why| format!("The device did not take the check `{check_id}`: {why}"))?;
        // Its record is the device's, as every job's is, so every window of the app reads it.
        let recorded = record().transpose()?;
        let job = recorded.as_ref().map_or_else(
            || {
                let n = self.next.get();
                self.next.set(n.saturating_add(1));
                format!("approved-check-{n}")
            },
            |recorded| recorded.path().to_owned(),
        );
        let mut jobs = self.jobs.borrow_mut();
        forget_ended(&mut jobs);
        jobs.insert(
            job.clone(),
            Job {
                invocation: served.invocation,
                call,
                heard: Heard::default(),
                answers: None,
                told: 0,
                settled: false,
                recorded,
            },
        );
        Ok(job)
    }

    /// What the dispatch said of a check's call: how far it got, and how it ended. Whether
    /// `message` was about one.
    pub fn heard(&self, message: &RemoteActionMessage) -> bool {
        let (call, update) = match message {
            RemoteActionMessage::Receipt(receipt) => (receipt.invocation, None),
            RemoteActionMessage::Update {
                invocation, state, ..
            } => (*invocation, Some(state.as_ref().map(|state| &state.update))),
            _ => return false,
        };
        let mut jobs = self.jobs.borrow_mut();
        let Some(job) = jobs.values_mut().find(|job| job.call == call) else {
            return false;
        };
        if job.heard.end.is_some() {
            return true;
        }
        let follows = match (message, update) {
            (RemoteActionMessage::Receipt(receipt), _) => match &receipt.status {
                ReceiptStatus::Accepted => true,
                ReceiptStatus::Completed { output } => {
                    job.heard.end = Some(observed(output));
                    false
                }
                ReceiptStatus::Failed { error } => {
                    job.heard.end = Some(End::Failed(error.message.clone()));
                    false
                }
                ReceiptStatus::Rejected { error } => {
                    job.heard.end = Some(End::Failed(error.said()));
                    false
                }
                ReceiptStatus::Cancelled { .. } => {
                    job.heard.end = Some(End::Cancelled);
                    false
                }
            },
            (_, Some(None)) => {
                job.heard.end = Some(End::Failed(
                    "The device no longer knows the check".to_owned(),
                ));
                false
            }
            (_, Some(Some(update))) => match update {
                InvocationUpdate::Progress {
                    phase,
                    completed,
                    total,
                    ..
                } => {
                    job.heard.reports = job.heard.reports.saturating_add(1);
                    job.heard.latest = Some((phase.clone(), *completed, *total));
                    true
                }
                InvocationUpdate::Succeeded { output, .. } => {
                    job.heard.end = Some(observed(output));
                    false
                }
                InvocationUpdate::Failed { error, .. } => {
                    job.heard.end = Some(End::Failed(error.message.clone()));
                    false
                }
                InvocationUpdate::Cancelled { .. } => {
                    job.heard.end = Some(End::Cancelled);
                    false
                }
                _ => true,
            },
            (_, None) => false,
        };
        if let Some(recorded) = &job.recorded {
            let state = match &job.heard.end {
                None => tilolang_tilo::engine::State::Running,
                Some(End::Observed(_)) => tilolang_tilo::engine::State::Done,
                Some(End::Failed(_)) => tilolang_tilo::engine::State::Failed,
                Some(End::Cancelled) => tilolang_tilo::engine::State::Cancelled,
            };
            recorded.moved(state, &status_of(&job.heard));
        }
        drop(jobs);
        if follows {
            let followed = self.send(RemoteActionMessage::Observe {
                correlation: 0,
                invocation: call,
                wait_ms: Some(FOLLOW_MS),
                seen: None,
            });
            if let Err(why) = followed {
                tracing::warn!(%why, "a running check is not followed");
            }
        }
        true
    }

    /// How far the check `job` has got, as JSON: its `state` (`running`, `done`, `failed` or
    /// `cancelled`), the newest phase the device reported with how much of how much is done, why
    /// it failed, and once done what the device observed: the commands' `outcome`, `exit_code`
    /// and `elapsed_ms`, the start of their `output` and `errors`, why the device could not
    /// supervise them (`failure`), and the whole `receipt`. `None` for a job that is not a check
    /// of this instance.
    #[must_use]
    pub fn status(&self, job: &str) -> Option<String> {
        let jobs = self.jobs.borrow();
        let status = status_of(&jobs.get(job)?.heard);
        drop(jobs);
        Some(status.to_string())
    }

    /// Stop the check `job`: false when it is no check of this instance or it had ended.
    pub fn cancel(&self, job: &str) -> bool {
        let call = self
            .jobs
            .borrow()
            .get(job)
            .filter(|job| job.heard.end.is_none())
            .map(|job| job.call);
        call.is_some_and(|call| self.stop(call))
    }

    /// Ask the device to stop the call `call`: whether the ask went out.
    fn stop(&self, call: InvocationId) -> bool {
        match self.send(RemoteActionMessage::Cancel { invocation: call }) {
            Ok(()) => true,
            Err(why) => {
                tracing::warn!(%why, "a check was not stopped");
                false
            }
        }
    }

    /// The run of the invocation the broker delivered as `delivery` ended, answering `output`:
    /// whether a check it started runs on, and then the invocation stays open and is answered
    /// when the check ends ([`Self::owed`]).
    pub fn continues(
        &self,
        delivery: u64,
        invocation: InvocationId,
        output: &ActionOutput,
    ) -> bool {
        let mut jobs = self.jobs.borrow_mut();
        let Some(job) = jobs
            .values_mut()
            .find(|job| job.invocation == invocation && job.answers.is_none() && !job.settled)
        else {
            return false;
        };
        job.answers = Some((delivery, output.clone()));
        true
    }

    /// The run of `invocation` failed after it started a check: the check stops with it, and
    /// answers nobody.
    pub fn abandon(&self, invocation: InvocationId) {
        let calls: Vec<InvocationId> = self
            .jobs
            .borrow_mut()
            .values_mut()
            .filter(|job| job.invocation == invocation && job.answers.is_none())
            .map(|job| {
                job.settled = true;
                job.call
            })
            .collect();
        for call in calls {
            self.stop(call);
        }
    }

    /// The broker cancelled the delivery `delivery`: the check that is its work stops, and the
    /// invocation is answered as cancelled once it has. Whether one ran.
    pub fn cancel_delivery(&self, delivery: u64) -> bool {
        let call = self
            .jobs
            .borrow()
            .values()
            .find(|job| {
                job.answers
                    .as_ref()
                    .is_some_and(|(answered, _)| *answered == delivery)
                    && !job.settled
                    && job.heard.end.is_none()
            })
            .map(|job| job.call);
        call.is_some_and(|call| self.stop(call))
    }

    /// What the broker is owed now, by delivery: each check's progress since the last turn as
    /// its invocation's progress, then its end.
    pub fn owed(&self) -> Vec<(u64, InvocationUpdate)> {
        let mut owed = Vec::new();
        for job in self.jobs.borrow_mut().values_mut() {
            if job.settled {
                continue;
            }
            let Some((delivery, output)) = job.answers.clone() else {
                continue;
            };
            if job.heard.reports > job.told
                && let Some((phase, completed, total)) = &job.heard.latest
            {
                owed.push((
                    delivery,
                    InvocationUpdate::Progress {
                        invocation: job.invocation,
                        sequence: job.heard.reports,
                        phase: phase.clone(),
                        completed: *completed,
                        total: *total,
                        message: None,
                    },
                ));
            }
            job.told = job.heard.reports;
            let Some(end) = job.heard.end.clone() else {
                continue;
            };
            job.settled = true;
            let invocation = job.invocation;
            owed.push((
                delivery,
                match end {
                    End::Observed(receipt) => InvocationUpdate::Succeeded {
                        invocation,
                        output: ActionOutput {
                            value: serde_json::to_value(&*receipt).ok(),
                            ..output
                        },
                    },
                    End::Failed(message) => InvocationUpdate::Failed {
                        invocation,
                        error: ActionFailure {
                            code: "action_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    },
                    End::Cancelled => InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("The approved check was stopped".to_owned()),
                    },
                },
            ));
        }
        owed
    }
}

/// What `heard` of a check says of how far it got and how it ended, as its record's fields.
fn status_of(heard: &Heard) -> serde_json::Value {
    let (phase, completed, total) = heard.latest.clone().unwrap_or_default();
    let mut status = serde_json::Map::new();
    let mut set = |key: &str, value: serde_json::Value| {
        status.insert(key.to_owned(), value);
    };
    set("state", "running".into());
    set("phase", phase.into());
    set("completed", completed.into());
    set("total", total.into());
    match &heard.end {
        None => {}
        Some(End::Observed(receipt)) => match serde_json::to_value(receipt) {
            Ok(observed) => {
                set("state", "done".into());
                set(
                    "outcome",
                    observed.get("outcome").cloned().unwrap_or_default(),
                );
                set("exit_code", receipt.exit_code.into());
                set("elapsed_ms", receipt.elapsed_ms.into());
                set("output", receipt.stdout.preview.clone().into());
                set("errors", receipt.stderr.preview.clone().into());
                set(
                    "failure",
                    receipt
                        .failure
                        .as_ref()
                        .map_or_else(String::new, |failure| failure.preview.clone())
                        .into(),
                );
                set("receipt", observed);
            }
            Err(error) => {
                set("state", "failed".into());
                set("error", format!("the receipt did not read: {error}").into());
            }
        },
        Some(End::Failed(why)) => {
            set("state", "failed".into());
            set("error", why.clone().into());
        }
        Some(End::Cancelled) => set("state", "cancelled".into()),
    }
    serde_json::Value::Object(status)
}

/// What the device answered a check with: the receipt of what it observed, or why it is none.
fn observed(output: &ActionOutput) -> End {
    let Some(value) = output.value.clone() else {
        return End::Failed("The device answered the check with no receipt".to_owned());
    };
    match serde_json::from_value::<Receipt>(value) {
        Ok(receipt) => End::Observed(Box::new(receipt)),
        Err(error) => End::Failed(format!(
            "The device's receipt of the check does not read: {error}"
        )),
    }
}

/// Forget the checks that ended longest ago once more than [`ENDED_KEPT`] are kept: their
/// invocations were answered, and the app read how they ended.
fn forget_ended(jobs: &mut BTreeMap<String, Job>) {
    let ended: Vec<String> = jobs
        .iter()
        .filter(|(_, job)| job.settled)
        .map(|(id, _)| id.clone())
        .collect();
    let excess = ended.len().saturating_sub(ENDED_KEPT);
    for id in ended.into_iter().take(excess) {
        jobs.remove(&id);
    }
}
