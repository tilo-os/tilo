//! One installation's firings: one at a time, in event order, retried
//! with backoff when the failure was transient, dead-lettered when it was
//! a refusal or kept failing.

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::ActionHostError;

use super::{Firing, MAX_QUEUED_FIRINGS};
use crate::run::{RunOutcome, RunReport, StepFailure, StepStatus};

pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(600);
pub const MAX_ATTEMPTS: u32 = 8;
/// The reason a firing dropped for overflow is dead-lettered with.
pub const FELL_BEHIND: &str = "fell behind";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    pub firing: Firing,
    /// Runs already tried.
    pub attempts: u32,
    pub not_before_ms: i64,
}

/// A firing that will not run again on its own: the event, the step that
/// stopped it and why. "Run again" invokes the bot with the same event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeadLetter {
    pub firing: Firing,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    pub reason: String,
    pub at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClass {
    /// The service or host was unavailable or slow: try again later.
    Transient,
    /// Deny, an invalid argument, an app's refusal: trying again changes
    /// nothing.
    Refusal,
}

/// Whether trying a failed run again could go differently.
#[must_use]
pub fn classify(failure: &StepFailure) -> FailureClass {
    match failure {
        StepFailure::Unavailable { .. } | StepFailure::Abandoned => FailureClass::Transient,
        StepFailure::Host { error } if matches!(**error, ActionHostError::Unavailable { .. }) => {
            FailureClass::Transient
        }
        StepFailure::Action { error } if error.retryable => FailureClass::Transient,
        _ => FailureClass::Refusal,
    }
}

/// The wait after attempt `attempt`: 1 s, doubling, at most 10 min.
#[must_use]
pub fn backoff(attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(20);
    BACKOFF_FIRST
        .checked_mul(1_u32 << doublings)
        .unwrap_or(BACKOFF_MAX)
        .min(BACKOFF_MAX)
}

/// How the head's run settled.
#[derive(Clone, Debug, PartialEq)]
pub enum Finished {
    Done,
    Retry { at_ms: i64 },
    DeadLettered(Box<DeadLetter>),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FiringQueue {
    queued: VecDeque<Queued>,
}

impl FiringQueue {
    /// Queue `firing` behind the others. A run already queued is not queued
    /// again. Beyond [`MAX_QUEUED_FIRINGS`] the oldest is dropped and
    /// returned dead-lettered as [`FELL_BEHIND`].
    pub fn push(&mut self, firing: Firing, now_ms: i64) -> Option<DeadLetter> {
        if self
            .queued
            .iter()
            .any(|queued| queued.firing.run == firing.run)
        {
            return None;
        }
        self.queued.push_back(Queued {
            firing,
            attempts: 0,
            not_before_ms: now_ms,
        });
        if self.queued.len() > MAX_QUEUED_FIRINGS {
            return self.dead_letter_head(FELL_BEHIND, now_ms);
        }
        None
    }

    /// The head, when it is due. Later firings never overtake it, so a
    /// mirror never applies an edit before the post it edits.
    #[must_use]
    pub fn next_due(&self, now_ms: i64) -> Option<&Queued> {
        self.queued
            .front()
            .filter(|queued| queued.not_before_ms <= now_ms)
    }

    /// When the head becomes due.
    #[must_use]
    pub fn wake_at_ms(&self) -> Option<i64> {
        self.queued.front().map(|queued| queued.not_before_ms)
    }

    /// Settle the head with how its run went.
    pub fn finished(&mut self, report: &RunReport, now_ms: i64) -> Finished {
        let Some(head) = self.queued.front_mut() else {
            return Finished::Done;
        };
        if head.firing.run != report.run {
            return Finished::Done;
        }
        let RunOutcome::Failed { failure, .. } = &report.outcome else {
            self.queued.pop_front();
            return Finished::Done;
        };
        head.attempts = head.attempts.saturating_add(1);
        if classify(failure) == FailureClass::Transient && head.attempts < MAX_ATTEMPTS {
            let wait = i64::try_from(backoff(head.attempts).as_millis()).unwrap_or(i64::MAX);
            head.not_before_ms = now_ms.saturating_add(wait);
            return Finished::Retry {
                at_ms: head.not_before_ms,
            };
        }
        let step = report.steps.iter().rev().find_map(|record| {
            matches!(record.status, StepStatus::Failed { .. }).then(|| record.step.clone())
        });
        let Some(queued) = self.queued.pop_front() else {
            return Finished::Done;
        };
        Finished::DeadLettered(Box::new(DeadLetter {
            firing: queued.firing,
            step,
            reason: failure.to_string(),
            at_ms: now_ms,
        }))
    }

    /// Settle the head when the host could not run it at all (its records
    /// were unreadable, or what it changed could not be published): tried
    /// again with backoff like a transient failure, dead-lettered with
    /// `reason` once [`MAX_ATTEMPTS`] are spent, so one firing the host
    /// cannot run never holds up the ones behind it for good.
    pub fn not_run(&mut self, reason: &str, now_ms: i64) -> Finished {
        let Some(head) = self.queued.front_mut() else {
            return Finished::Done;
        };
        head.attempts = head.attempts.saturating_add(1);
        if head.attempts < MAX_ATTEMPTS {
            let wait = i64::try_from(backoff(head.attempts).as_millis()).unwrap_or(i64::MAX);
            head.not_before_ms = now_ms.saturating_add(wait);
            return Finished::Retry {
                at_ms: head.not_before_ms,
            };
        }
        self.dead_letter_head(reason, now_ms)
            .map_or(Finished::Done, |dead| {
                Finished::DeadLettered(Box::new(dead))
            })
    }

    /// Drop the head into a dead letter with `reason`.
    pub fn dead_letter_head(&mut self, reason: &str, now_ms: i64) -> Option<DeadLetter> {
        self.queued.pop_front().map(|queued| DeadLetter {
            firing: queued.firing,
            step: None,
            reason: reason.to_owned(),
            at_ms: now_ms,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.queued.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StepPath;
    use crate::run::{RunId, RunMode, RunOutcome, RunReport, StepFailure};
    use crate::shortcut::TriggerSpec;

    fn firing(n: u8) -> Firing {
        Firing {
            firer: "bot:b1".into(),
            trigger_index: 0,
            trigger: TriggerSpec::IncomingMessage {
                app_id: None,
                from: None,
            },
            run: RunId(uuid::Uuid::from_bytes([n; 16])),
            inputs: serde_json::Map::new(),
            cause: None,
            event: format!("e{n}"),
        }
    }

    fn report(run: RunId, outcome: RunOutcome) -> RunReport {
        RunReport {
            run,
            shortcut: "s".into(),
            trigger: "t".into(),
            mode: RunMode::Unattended,
            steps: vec![],
            output: None,
            outcome,
        }
    }

    fn failed(failure: StepFailure) -> RunOutcome {
        RunOutcome::Failed {
            path: StepPath::root().child(0),
            failure,
        }
    }

    #[test]
    fn firings_run_one_at_a_time_in_order() {
        let mut queue = FiringQueue::default();
        for n in 1..=3 {
            assert!(queue.push(firing(n), 0).is_none());
        }
        assert_eq!(queue.next_due(0).unwrap().firing.event, "e1");
        assert!(matches!(
            queue.finished(&report(firing(1).run, RunOutcome::Completed), 0),
            Finished::Done
        ));
        assert_eq!(queue.next_due(0).unwrap().firing.event, "e2");
    }

    #[test]
    fn a_firing_the_host_cannot_run_backs_off_then_is_dead_lettered() {
        let mut queue = FiringQueue::default();
        queue.push(firing(1), 0);
        queue.push(firing(2), 0);
        for _ in 1..MAX_ATTEMPTS {
            assert!(matches!(
                queue.not_run("publish failed", 0),
                Finished::Retry { .. }
            ));
            assert!(queue.next_due(0).is_none(), "it waits before the next try");
        }
        let Finished::DeadLettered(dead) = queue.not_run("publish failed", 0) else {
            panic!("dead-lettered once its attempts are spent");
        };
        assert_eq!(
            (dead.firing.event.as_str(), dead.reason.as_str()),
            ("e1", "publish failed")
        );
        assert_eq!(queue.next_due(0).unwrap().firing.event, "e2");
    }

    #[test]
    fn the_same_run_queued_twice_is_kept_once() {
        let mut queue = FiringQueue::default();
        queue.push(firing(1), 0);
        queue.push(firing(1), 0);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn overflow_drops_the_oldest_into_dead_letters() {
        let mut queue = FiringQueue::default();
        let mut dropped = Vec::new();
        for n in 0..=u8::try_from(MAX_QUEUED_FIRINGS).unwrap() {
            dropped.extend(queue.push(firing(n), 7));
        }
        assert_eq!(queue.len(), MAX_QUEUED_FIRINGS);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].firing.event, "e0");
        assert_eq!(dropped[0].reason, FELL_BEHIND);
        assert_eq!(queue.next_due(7).unwrap().firing.event, "e1");
    }

    #[test]
    fn a_transient_failure_retries_with_backoff_and_holds_later_firings() {
        let mut queue = FiringQueue::default();
        queue.push(firing(1), 0);
        queue.push(firing(2), 0);
        let unavailable = failed(StepFailure::Host {
            error: Box::new(ActionHostError::Unavailable {
                reason: "down".into(),
            }),
        });
        let Finished::Retry { at_ms } =
            queue.finished(&report(firing(1).run, unavailable.clone()), 1_000)
        else {
            panic!("retried");
        };
        assert_eq!(at_ms, 2_000);
        assert!(
            queue.next_due(1_500).is_none(),
            "later firings wait for the retry"
        );
        assert_eq!(queue.next_due(2_000).unwrap().firing.event, "e1");
        let mut now = 2_000;
        let mut last = Finished::Done;
        for _ in 2..=MAX_ATTEMPTS {
            last = queue.finished(&report(firing(1).run, unavailable.clone()), now);
            if let Finished::Retry { at_ms } = last {
                now = at_ms;
            }
        }
        let Finished::DeadLettered(letter) = last else {
            panic!("a poison event is dead-lettered");
        };
        assert_eq!(letter.firing.event, "e1");
        assert_eq!(queue.next_due(now).unwrap().firing.event, "e2");
    }

    #[test]
    fn a_refusal_is_dead_lettered_at_once_with_its_step() {
        let mut queue = FiringQueue::default();
        queue.push(firing(1), 0);
        let outside = || StepFailure::OutsideGrant {
            app_id: "a".into(),
            action: "b".into(),
        };
        let mut denied = report(firing(1).run, failed(outside()));
        denied.steps.push(crate::run::StepRecord {
            path: StepPath::root().child(0),
            step: "post".into(),
            invocation: None,
            status: StepStatus::Failed { failure: outside() },
        });
        let Finished::DeadLettered(letter) = queue.finished(&denied, 5) else {
            panic!("dead-lettered");
        };
        assert_eq!(letter.step.as_deref(), Some("post"));
        assert!(letter.reason.contains("grant"));
        assert!(queue.is_empty());
    }

    #[test]
    fn backoff_doubles_from_one_second_to_ten_minutes() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(4), Duration::from_secs(8));
        assert_eq!(backoff(20), Duration::from_secs(600));
    }

    #[test]
    fn failures_are_classified_as_the_spec_says() {
        let host = |error| StepFailure::Host {
            error: Box::new(error),
        };
        assert_eq!(
            classify(&host(ActionHostError::Unavailable {
                reason: String::new()
            })),
            FailureClass::Transient
        );
        assert_eq!(
            classify(&StepFailure::Unavailable {
                code: "x".into(),
                reason: String::new()
            }),
            FailureClass::Transient
        );
        assert_eq!(classify(&StepFailure::Abandoned), FailureClass::Transient);
        assert_eq!(
            classify(&StepFailure::OutsideGrant {
                app_id: String::new(),
                action: String::new()
            }),
            FailureClass::Refusal
        );
        assert_eq!(
            classify(&StepFailure::AsksFirst {
                app_id: String::new(),
                action: String::new()
            }),
            FailureClass::Refusal
        );
        assert_eq!(
            classify(&StepFailure::Arguments {
                reason: String::new()
            }),
            FailureClass::Refusal
        );
        assert_eq!(
            classify(&host(ActionHostError::NotPermitted)),
            FailureClass::Refusal
        );
    }

    #[test]
    fn the_queue_survives_serialisation() {
        let mut queue = FiringQueue::default();
        queue.push(firing(1), 0);
        let text = serde_json::to_string(&queue).unwrap();
        let back: FiringQueue = serde_json::from_str(&text).unwrap();
        assert_eq!(back.next_due(0).unwrap().firing, firing(1));
    }
}
