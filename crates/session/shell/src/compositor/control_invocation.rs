//! An `actions_invoke` a control connection answers later: once it is
//! admitted, or — asked with `wait_ms` — once it settles.
//!
//! Nothing here sleeps or polls on a timer. An admission wakes the loop
//! when its answer arrives, and a settling job when the broker records a
//! change to it ([`invocation_wake`]); the loop then polls the connection's
//! [`ControlInvocation`] again. [`ControlInvocation::recheck_at`] is the one
//! deadline: when a `wait_ms` runs out, or a fallback for an answer that
//! cannot signal (an admission's own timeout).

use std::time::{Duration, Instant};

use crate::shell::search::invocation_wake;
use crate::shell::search::{ActionReceipt, PendingInvocation};
use crate::shell::test_control;

/// The longest a reply waits for its invocation to settle, whatever
/// `wait_ms` asked: the MCP door's bound.
pub(crate) const MAX_SETTLE_WAIT: Duration = Duration::from_secs(30);

/// How long an invocation that may not signal its answer is left before
/// the loop looks again.
const RECHECK: Duration = Duration::from_secs(1);

/// How often one poll reads a job that keeps changing while it is read.
const MAX_READS: usize = 4;

/// How long `command` asks its reply to wait for the invocation to settle:
/// an `actions_invoke`, or an `actions_status` read of a job, with
/// `wait_ms`. Only an invocation run here can be followed; one sent to an
/// enrolled device answers once it is queued.
pub(crate) fn settle_window(command: &test_control::Command) -> Option<Duration> {
    match command {
        test_control::Command::ActionsInvoke {
            wait_ms: Some(wait_ms),
            device: None,
            ..
        }
        | test_control::Command::ActionsStatus {
            wait_ms: Some(wait_ms),
            ..
        } => Some(Duration::from_millis(*wait_ms).min(MAX_SETTLE_WAIT)),
        _ => None,
    }
}

/// The answer to an invocation: what was invoked and its latest receipt.
pub(crate) fn invocation_response(
    action: &serde_json::Value,
    receipt: &ActionReceipt,
) -> test_control::Response {
    test_control::Response::Answer(serde_json::json!({
        "action": action,
        "receipt": receipt,
    }))
}

/// Where a waiting reply reads its job's progress.
pub(crate) trait InvocationProgress {
    /// `job`'s latest update, read exactly as `actions_status` reads it;
    /// `None` when it cannot be read.
    fn status(&mut self, job: u64) -> Option<ActionReceipt>;

    /// Settles on the next change to any invocation's status after this
    /// call; `None` where no change can be watched.
    fn next_change(&self) -> Option<tilo_actions::remote::Pending<()>>;
}

/// An invocation whose answer a control connection holds back.
pub(crate) struct ControlInvocation {
    /// What was invoked, as the request's dispatch resolved it.
    action: serde_json::Value,
    stage: Stage,
    /// With `wait_ms`: the latest the reply waits for the job to settle.
    settle_by: Option<Instant>,
}

enum Stage {
    /// Not admitted yet.
    Admission(PendingInvocation),
    /// Admitted as a broker job that had not settled when last read.
    Settling(Settling),
}

struct Settling {
    job: u64,
    /// The newest update read so far.
    latest: ActionReceipt,
    /// Settles on the broker's next status change after `latest` was read;
    /// `None` before the first read, or where no change can be watched.
    change: Option<tilo_actions::remote::Pending<()>>,
}

/// What polling a [`ControlInvocation`] found.
pub(crate) enum ControlInvocationPoll {
    Waiting(ControlInvocation),
    Ready(test_control::Response),
}

impl ControlInvocation {
    /// An invocation still waiting on its admission `ticket`.
    pub(crate) fn admitting(
        ticket: PendingInvocation,
        action: serde_json::Value,
        settle_by: Option<Instant>,
    ) -> Self {
        Self {
            action,
            stage: Stage::Admission(ticket),
            settle_by,
        }
    }

    /// An admitted invocation. Its `receipt` is the answer, unless the
    /// request waits until `settle_by` and the job has not settled: then the
    /// job is followed.
    ///
    /// # Errors
    ///
    /// The answer to send now, when there is nothing to wait for.
    #[allow(
        clippy::result_large_err,
        reason = "the answer is the wire Response the transport writes back as is"
    )]
    pub(crate) fn admitted(
        action: serde_json::Value,
        receipt: ActionReceipt,
        settle_by: Option<Instant>,
    ) -> Result<Self, test_control::Response> {
        match (settle_by, receipt.broker_invocation_id) {
            (Some(by), Some(job)) if !receipt.update.is_terminal() && Instant::now() < by => {
                Ok(Self {
                    action,
                    stage: Stage::Settling(Settling {
                        job,
                        latest: receipt,
                        change: None,
                    }),
                    settle_by,
                })
            }
            _ => Err(invocation_response(&action, &receipt)),
        }
    }

    /// Answer if it can be answered now. A job is read again only once the
    /// broker reported a change since the last read.
    pub(crate) fn poll(self, progress: &mut dyn InvocationProgress) -> ControlInvocationPoll {
        let Self {
            action,
            stage,
            settle_by,
        } = self;
        let mut settling = match stage {
            Stage::Admission(ticket) => {
                return match ticket() {
                    Ok(None) => ControlInvocationPoll::Waiting(Self {
                        action,
                        stage: Stage::Admission(ticket),
                        settle_by,
                    }),
                    Ok(Some(receipt)) => match Self::admitted(action, receipt, settle_by) {
                        // Read the job now: it may have settled already.
                        Ok(admitted) => admitted.poll(progress),
                        Err(answer) => ControlInvocationPoll::Ready(answer),
                    },
                    Err(error) => ControlInvocationPoll::Ready(test_control::Response::Err {
                        code: error.code().to_string(),
                        message: error.to_string(),
                    }),
                };
            }
            Stage::Settling(settling) => settling,
        };
        // No longer kept, or no longer this caller's: answer with what is
        // known rather than invent an outcome.
        if settling.moved() && !settling.read(progress) {
            return ControlInvocationPoll::Ready(invocation_response(&action, &settling.latest));
        }
        if settling.latest.update.is_terminal() || settle_by.is_none_or(|by| Instant::now() >= by) {
            return ControlInvocationPoll::Ready(invocation_response(&action, &settling.latest));
        }
        ControlInvocationPoll::Waiting(Self {
            action,
            stage: Stage::Settling(settling),
            settle_by,
        })
    }

    /// When the loop must poll this again although nothing woke it: when
    /// the wait runs out, or soon, for an answer that may not signal.
    pub(crate) fn recheck_at(&self, now: Instant) -> Option<Instant> {
        let soon = now.checked_add(RECHECK);
        match &self.stage {
            // An admission wakes the loop when it is answered; its own
            // timeout does not.
            Stage::Admission(_) => soon,
            Stage::Settling(settling) if settling.change.is_some() => self.settle_by,
            Stage::Settling(_) => [self.settle_by, soon].into_iter().flatten().min(),
        }
    }
}

impl Settling {
    /// Whether the broker reported a change since the last read, or there
    /// has been no read yet. While it has not, the loop is woken when it
    /// does.
    fn moved(&mut self) -> bool {
        let waker = invocation_wake::waker();
        self.change.as_mut().is_none_or(|change| {
            !matches!(
                invocation_wake::poll(change, &waker),
                tilo_actions::remote::PendingPoll::Waiting
            )
        })
    }

    /// Read the job afresh, leaving the change after the read watched.
    /// `false` when it cannot be read.
    fn read(&mut self, progress: &mut dyn InvocationProgress) -> bool {
        for _ in 0..MAX_READS {
            // Watched from before the read: a change racing the read is
            // seen below instead of slipping past.
            self.change = progress.next_change();
            match progress.status(self.job) {
                Some(receipt) => self.latest = receipt,
                None => return false,
            }
            if self.latest.update.is_terminal() || self.change.is_none() || !self.moved() {
                return true;
            }
        }
        // Still changing while read: read again on the next wake or recheck.
        self.change = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::{Completer, pending};

    use super::*;

    /// A broker job whose status the test sets, with no change feed.
    #[derive(Default)]
    struct Job {
        status: Option<InvocationUpdate>,
        reads: usize,
    }

    impl InvocationProgress for Job {
        fn status(&mut self, job: u64) -> Option<ActionReceipt> {
            self.reads = self.reads.saturating_add(1);
            self.status.clone().map(|update| ActionReceipt {
                broker_invocation_id: Some(job),
                update,
            })
        }

        fn next_change(&self) -> Option<tilo_actions::remote::Pending<()>> {
            None
        }
    }

    /// The same job, with a change feed: the signals the poll armed.
    struct Watched(Job, std::cell::RefCell<Vec<Completer<()>>>);

    impl InvocationProgress for Watched {
        fn status(&mut self, job: u64) -> Option<ActionReceipt> {
            self.0.status(job)
        }

        fn next_change(&self) -> Option<tilo_actions::remote::Pending<()>> {
            let (completer, change) = pending();
            self.1.borrow_mut().push(completer);
            Some(change)
        }
    }

    fn accepted() -> InvocationUpdate {
        InvocationUpdate::Accepted {
            invocation: tilo_actions::InvocationId::new(),
        }
    }

    fn succeeded() -> InvocationUpdate {
        InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId::new(),
            output: tilo_actions::ActionOutput::default(),
        }
    }

    fn receipt(job: Option<u64>, update: InvocationUpdate) -> ActionReceipt {
        ActionReceipt {
            broker_invocation_id: job,
            update,
        }
    }

    fn state(response: &test_control::Response) -> String {
        let test_control::Response::Answer(answer) = response else {
            panic!("expected an answer, got {response:?}");
        };
        answer["receipt"]["update"]["state"]
            .as_str()
            .expect("receipt state")
            .to_string()
    }

    fn later(wait: Duration) -> Option<Instant> {
        Instant::now().checked_add(wait)
    }

    fn followed(receipt: ActionReceipt, settle_by: Option<Instant>) -> ControlInvocation {
        match ControlInvocation::admitted(serde_json::json!("a"), receipt, settle_by) {
            Ok(followed) => followed,
            Err(answer) => panic!("an unsettled job is followed, not answered: {answer:?}"),
        }
    }

    #[test]
    fn wait_ms_is_bounded_and_ignored_for_a_device() {
        let invoke = |wait_ms, device: Option<&str>| test_control::Command::ActionsInvoke {
            provider: "p".into(),
            id: "i".into(),
            args: serde_json::Map::new(),
            device: device.map(str::to_string),
            wait_ms,
        };
        assert_eq!(settle_window(&invoke(None, None)), None);
        assert_eq!(
            settle_window(&invoke(Some(250), None)),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            settle_window(&invoke(Some(u64::MAX), None)),
            Some(MAX_SETTLE_WAIT)
        );
        assert_eq!(settle_window(&invoke(Some(250), Some("device"))), None);
        // A job read waits the same way, through the same bound.
        let status = |wait_ms| test_control::Command::ActionsStatus {
            invocation_id: 7,
            wait_ms,
        };
        assert_eq!(settle_window(&status(None)), None);
        assert_eq!(
            settle_window(&status(Some(40))),
            Some(Duration::from_millis(40))
        );
        assert_eq!(
            settle_window(&status(Some(u64::MAX))),
            Some(MAX_SETTLE_WAIT)
        );
    }

    #[test]
    fn without_a_wait_the_admission_receipt_is_the_answer() {
        let Err(answer) =
            ControlInvocation::admitted(serde_json::json!("a"), receipt(Some(4), accepted()), None)
        else {
            panic!("nothing to wait for");
        };
        assert_eq!(state(&answer), "accepted");
        // A settled or job-less receipt has nothing to follow either.
        for settled in [receipt(Some(4), succeeded()), receipt(None, accepted())] {
            assert!(
                ControlInvocation::admitted(
                    serde_json::json!("a"),
                    settled,
                    later(Duration::from_secs(5))
                )
                .is_err()
            );
        }
    }

    #[test]
    fn a_waiting_reply_answers_the_settled_job_and_reads_only_after_a_change() {
        let ticket: PendingInvocation = Rc::new(|| Ok(Some(receipt(Some(9), accepted()))));
        let invocation = ControlInvocation::admitting(
            ticket,
            serde_json::json!("a"),
            later(Duration::from_secs(30)),
        );
        let mut job = Watched(
            Job {
                status: Some(accepted()),
                ..Job::default()
            },
            std::cell::RefCell::default(),
        );
        let ControlInvocationPoll::Waiting(invocation) = invocation.poll(&mut job) else {
            panic!("an accepted job keeps the reply waiting");
        };
        assert_eq!(job.0.reads, 1, "admission reads the job at once");
        assert_eq!(job.1.borrow().len(), 1, "and arms a change signal first");
        assert!(
            invocation.recheck_at(Instant::now()) > later(Duration::from_secs(5)),
            "a watched job is not rechecked on a timer"
        );

        let ControlInvocationPoll::Waiting(invocation) = invocation.poll(&mut job) else {
            panic!("nothing changed");
        };
        assert_eq!(job.0.reads, 1, "no change, no read");

        job.0.status = Some(succeeded());
        job.1.borrow_mut().remove(0).complete(());
        let ControlInvocationPoll::Ready(answer) = invocation.poll(&mut job) else {
            panic!("a settled job answers");
        };
        assert_eq!(job.0.reads, 2);
        assert_eq!(state(&answer), "succeeded");
        let test_control::Response::Answer(answer) = answer else {
            unreachable!()
        };
        assert_eq!(answer["action"], "a");
        assert_eq!(answer["receipt"]["broker_invocation_id"], 9);
    }

    #[test]
    fn a_waiting_reply_answers_what_it_knows_when_the_wait_runs_out_or_the_job_is_gone() {
        let expired = followed(
            receipt(Some(3), accepted()),
            later(Duration::from_millis(20)),
        );
        let mut running = Job {
            status: Some(accepted()),
            ..Job::default()
        };
        let ControlInvocationPoll::Waiting(expired) = expired.poll(&mut running) else {
            panic!("still within the wait");
        };
        let due = expired
            .recheck_at(Instant::now())
            .expect("an unwatched wait has a deadline");
        assert!(due <= later(Duration::from_secs(1)).expect("later"));
        std::thread::sleep(Duration::from_millis(30));
        let ControlInvocationPoll::Ready(answer) = expired.poll(&mut running) else {
            panic!("the wait ran out");
        };
        assert_eq!(state(&answer), "accepted", "the latest update, unsettled");

        let gone = followed(receipt(Some(3), accepted()), later(Duration::from_secs(30)));
        let ControlInvocationPoll::Ready(answer) = gone.poll(&mut Job::default()) else {
            panic!("an unreadable job answers at once");
        };
        assert_eq!(state(&answer), "accepted");
    }

    #[test]
    fn an_unanswered_admission_is_rechecked_and_a_refusal_is_the_answer() {
        let ticket: PendingInvocation = Rc::new(|| Ok(None));
        let waiting = ControlInvocation::admitting(ticket, serde_json::json!("a"), None);
        let now = Instant::now();
        assert_eq!(waiting.recheck_at(now), now.checked_add(RECHECK));
        let ControlInvocationPoll::Waiting(_) = waiting.poll(&mut Job::default()) else {
            panic!("no answer yet");
        };

        let refused: PendingInvocation = Rc::new(|| {
            Err(crate::shell::search::action_api::ActionError::Refused(
                "no".into(),
            ))
        });
        let ControlInvocationPoll::Ready(test_control::Response::Err { code, .. }) =
            ControlInvocation::admitting(refused, serde_json::json!("a"), None)
                .poll(&mut Job::default())
        else {
            panic!("a refusal answers");
        };
        assert_eq!(code, "action_failed");
    }
}
