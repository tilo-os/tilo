//! Checks the project's host approved, run for an app's `run-approved-check` (`engine.check`).
//!
//! A tilolang app reaches a check the way a Rust app does: the request goes out through
//! [`collaboration::approved_checks::enqueue_reported`] on the route channel the instance joined,
//! naming the invocation the broker served the action for. The broker recovers who asked and
//! what for from that invocation, rechecks it while the check runs, and the session's project
//! host picks the commands, the sources and the bounds. Nothing the app passes but the check's id
//! reaches the host.
//!
//! The check is the served invocation's work: the action's code ends once it started it, and the
//! invocation stays open until the check ends, answered then with the receipt of what the host
//! observed. What the host reports on the way is the invocation's progress, and the app reads
//! the same through `engine.status(job)`; `engine.cancel(job)`, or the invocation's own cancel,
//! stops it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};

use collaboration::approved_checks::{Receipt, ReportObservation, Request};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilolang_tilo::background::Waker;

/// Why no check starts where the instance runs on its device alone: no broker carries it.
pub const ON_THIS_DEVICE_ALONE: &str = "No project's approved checks run here: this app runs on \
     this device alone, with no workspace host to run them";

/// Why no check starts outside an invocation the workspace dispatched: the broker runs a check
/// only for one it holds.
pub const NOT_DISPATCHED: &str = "An approved check runs for an action the workspace dispatched: \
     run it from the app's actions, or ask an agent to";

/// Why no check starts from an action other than the app's `run-approved-check`.
pub const NOT_THE_CHECK_ACTION: &str =
    "Only the app's own run-approved-check action runs an approved check";

/// How many checks that ended are kept for the app to read before the oldest is forgotten.
const ENDED_KEPT: usize = 16;

/// The invocation the broker served whose code asks for a check: its id and the action it names.
pub struct Served<'a> {
    pub invocation: InvocationId,
    pub action: &'a str,
}

/// How a check ended.
#[derive(Clone, Debug)]
enum End {
    /// The host ran it and observed this.
    Observed(Box<Receipt>),
    /// It did not run, or the host could not observe it: why.
    Failed(String),
    /// It was stopped.
    Cancelled,
}

/// What a running check said so far.
#[derive(Default)]
struct Heard {
    /// How many reports were heard: the sequence of the newest.
    reports: u64,
    latest: Option<ReportObservation>,
    end: Option<End>,
}

/// One check, shared with the thread that hears it.
struct Shared {
    heard: Mutex<Heard>,
    stop: AtomicBool,
    thread: OnceLock<std::thread::Thread>,
}

impl Shared {
    fn heard(&self) -> std::sync::MutexGuard<'_, Heard> {
        self.heard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The host reported `observation`: the newest, one more in the sequence.
    fn said(&self, observation: ReportObservation) {
        let mut heard = self.heard();
        heard.reports = heard.reports.saturating_add(1);
        heard.latest = Some(observation);
    }
}

/// One check this instance started.
struct Job {
    invocation: InvocationId,
    shared: Arc<Shared>,
    /// The broker's delivery the check is the work of, and what the action's run answered, once
    /// that run ended: the invocation is answered with it when the check ends.
    answers: Option<(u64, ActionOutput)>,
    /// How many reports the broker was told as the invocation's progress.
    told: u64,
    /// Whether its end was answered, or answers nobody.
    settled: bool,
}

/// The checks one instance started, by the job id the app holds.
pub struct Checks {
    /// The route channel the instance joined, which the check's request goes out on; `None` for
    /// an instance with no broker.
    route: Option<String>,
    wake: Waker,
    jobs: RefCell<BTreeMap<String, Job>>,
    next: Cell<u64>,
}

impl Checks {
    /// The checks of an instance joined on the channel `route` (`None` with no broker), waking it
    /// with `wake` whenever one says something.
    #[must_use]
    pub fn new(route: Option<String>, wake: Waker) -> Self {
        Self {
            route,
            wake,
            jobs: RefCell::default(),
            next: Cell::new(1),
        }
    }

    /// Start the check `check_id` for the invocation `served` the broker is serving: the job id
    /// the app follows it by.
    ///
    /// # Errors
    ///
    /// Why it does not start: no broker, no served invocation, an action other than the app's
    /// `run-approved-check`, or the request refused before it left.
    pub fn start(&self, served: Option<Served<'_>>, check_id: &str) -> Result<String, String> {
        let route = self
            .route
            .as_deref()
            .ok_or_else(|| ON_THIS_DEVICE_ALONE.to_owned())?;
        let served = served.ok_or_else(|| NOT_DISPATCHED.to_owned())?;
        if served.action != collaboration::APPROVED_CHECK_ACTION {
            return Err(NOT_THE_CHECK_ACTION.to_owned());
        }
        let request = Request {
            version: 1,
            invocation: served.invocation.0.to_string(),
            check_id: check_id.to_owned(),
        };
        let (receipt, reports) = collaboration::approved_checks::enqueue_reported(route, request)
            .map_err(|why| {
            format!("The workspace's host did not take the check `{check_id}`: {why}")
        })?;
        let shared = Arc::new(Shared {
            heard: Mutex::default(),
            stop: AtomicBool::new(false),
            thread: OnceLock::new(),
        });
        let hearing = Arc::clone(&shared);
        let wake = Arc::clone(&self.wake);
        let thread = std::thread::Builder::new()
            .name("tilolang-approved-check".into())
            .spawn(move || {
                // Dropping the receivers once stopped tells the broker's queue its observer left,
                // which ends the host's work on the check.
                let (mut receipt, mut reports) = (receipt, reports);
                let end = block_on(async {
                    let mut open = true;
                    loop {
                        let step = {
                            let mut changed = std::pin::pin!(reports.changed());
                            std::future::poll_fn(|cx: &mut Context<'_>| {
                                if hearing.stop.load(Ordering::SeqCst) {
                                    return Poll::Ready(Step::Stopped);
                                }
                                if open && let Poll::Ready(said) = changed.as_mut().poll(cx) {
                                    return Poll::Ready(Step::Reported(said.is_ok()));
                                }
                                match Pin::new(&mut receipt).poll(cx) {
                                    Poll::Ready(Ok(ended)) => Poll::Ready(Step::Ended(ended)),
                                    Poll::Ready(Err(_)) => Poll::Ready(Step::Ended(Err(
                                        "The check's host stopped answering".to_owned(),
                                    ))),
                                    Poll::Pending => Poll::Pending,
                                }
                            })
                            .await
                        };
                        match step {
                            Step::Stopped => return End::Cancelled,
                            Step::Ended(Ok(observed)) => return End::Observed(Box::new(observed)),
                            Step::Ended(Err(why)) => return End::Failed(why),
                            Step::Reported(false) => open = false,
                            Step::Reported(true) => {
                                let said = reports.borrow_and_update().clone();
                                if let Some(report) = said {
                                    hearing.said(report.observation);
                                    wake();
                                }
                            }
                        }
                    }
                });
                drop((receipt, reports));
                hearing.heard().end = Some(end);
                wake();
            })
            .map_err(|error| format!("The check could not be followed: {error}"))?;
        let _ = shared.thread.set(thread.thread().clone());
        let n = self.next.get();
        self.next.set(n.saturating_add(1));
        let job = format!("approved-check-{n}");
        let mut jobs = self.jobs.borrow_mut();
        forget_ended(&mut jobs);
        jobs.insert(
            job.clone(),
            Job {
                invocation: served.invocation,
                shared,
                answers: None,
                told: 0,
                settled: false,
            },
        );
        Ok(job)
    }

    /// How far the check `job` has got, as JSON: its `state` (`running`, `done`, `failed` or
    /// `cancelled`), the newest phase the host reported with how much of how much is done, why
    /// it failed, and once done what the host observed: the commands' `outcome`, `exit_code` and
    /// `elapsed_ms`, the start of their `output` and `errors`, why the host could not supervise
    /// them (`failure`), and the whole `receipt`. `None` for a job that is not a check of this
    /// instance.
    #[must_use]
    pub fn status(&self, job: &str) -> Option<String> {
        let jobs = self.jobs.borrow();
        let heard = jobs.get(job)?.shared.heard();
        let (phase, completed, total) = match &heard.latest {
            Some(ReportObservation::Reported {
                phase,
                completed,
                total,
            }) => (phase.clone(), *completed, *total),
            Some(ReportObservation::Disabled { .. }) | None => (String::new(), None, None),
        };
        let mut status = serde_json::json!({
            "state": "running",
            "phase": phase,
            "completed": completed,
            "total": total,
        });
        match &heard.end {
            None => {}
            Some(End::Observed(receipt)) => {
                let observed = serde_json::to_value(receipt).expect("a receipt is JSON");
                status["state"] = "done".into();
                status["outcome"] = observed["outcome"].clone();
                status["exit_code"] = receipt.exit_code.into();
                status["elapsed_ms"] = receipt.elapsed_ms.into();
                status["output"] = receipt.stdout.preview.clone().into();
                status["errors"] = receipt.stderr.preview.clone().into();
                status["failure"] = receipt
                    .failure
                    .as_ref()
                    .map_or_else(String::new, |failure| failure.preview.clone())
                    .into();
                status["receipt"] = observed;
            }
            Some(End::Failed(why)) => {
                status["state"] = "failed".into();
                status["error"] = why.clone().into();
            }
            Some(End::Cancelled) => status["state"] = "cancelled".into(),
        }
        Some(status.to_string())
    }

    /// Stop the check `job`: false when it is no check of this instance or it had ended.
    pub fn cancel(&self, job: &str) -> bool {
        self.jobs
            .borrow()
            .get(job)
            .is_some_and(|job| stop(&job.shared))
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
        for job in self.jobs.borrow_mut().values_mut() {
            if job.invocation == invocation && job.answers.is_none() {
                stop(&job.shared);
                job.settled = true;
            }
        }
    }

    /// The broker cancelled the delivery `delivery`: the check that is its work stops, and the
    /// invocation is answered as cancelled once it has. Whether one ran.
    pub fn cancel_delivery(&self, delivery: u64) -> bool {
        self.jobs.borrow().values().any(|job| {
            job.answers
                .as_ref()
                .is_some_and(|(answered, _)| *answered == delivery)
                && !job.settled
                && stop(&job.shared)
        })
    }

    /// What the broker is owed now, by delivery: each check's reports since the last turn as its
    /// invocation's progress, then its end.
    pub fn owed(&self) -> Vec<(u64, InvocationUpdate)> {
        let mut owed = Vec::new();
        for job in self.jobs.borrow_mut().values_mut() {
            if job.settled {
                continue;
            }
            let Some((delivery, output)) = job.answers.clone() else {
                continue;
            };
            let heard = job.shared.heard();
            if heard.reports > job.told
                && let Some(ReportObservation::Reported {
                    phase,
                    completed,
                    total,
                }) = &heard.latest
            {
                owed.push((
                    delivery,
                    InvocationUpdate::Progress {
                        invocation: job.invocation,
                        sequence: heard.reports,
                        phase: phase.clone(),
                        completed: *completed,
                        total: *total,
                        message: None,
                    },
                ));
            }
            let reports = heard.reports;
            let end = heard.end.clone();
            drop(heard);
            job.told = reports;
            let Some(end) = end else {
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

/// Forget the checks that ended longest ago once more than [`ENDED_KEPT`] are kept: their
/// invocations were answered, and the app read how they ended.
fn forget_ended(jobs: &mut BTreeMap<String, Job>) {
    let ended: Vec<String> = jobs
        .iter()
        .filter(|(_, job)| job.settled || job.shared.heard().end.is_some() && job.answers.is_none())
        .map(|(id, _)| id.clone())
        .collect();
    let excess = ended.len().saturating_sub(ENDED_KEPT);
    for id in ended.into_iter().take(excess) {
        jobs.remove(&id);
    }
}

/// Ask the thread hearing `shared` to stop it: false when it had ended.
fn stop(shared: &Shared) -> bool {
    if shared.heard().end.is_some() {
        return false;
    }
    shared.stop.store(true, Ordering::SeqCst);
    if let Some(thread) = shared.thread.get() {
        thread.unpark();
    }
    true
}

/// What one wait for a check came to.
enum Step {
    Reported(bool),
    Ended(Result<Receipt, String>),
    Stopped,
}

/// Run `work` to its end on this thread, parking it until something it waits on wakes it.
fn block_on<F: Future>(work: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut work = std::pin::pin!(work);
    loop {
        if let Poll::Ready(done) = work.as_mut().poll(&mut cx) {
            return done;
        }
        std::thread::park();
    }
}
