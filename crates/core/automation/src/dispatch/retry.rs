//! Whether a triggered run that ended goes again.
//!
//! A run whose step was unavailable (no model yet, nothing to take what it hands over, a service
//! that is down) fires again after a wait that doubles, until its attempts are spent. Any other
//! end is final: a run that was refused or failed for good keeps its failed invocation record,
//! and that record is what is kept of it.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::ActionCallError;

use crate::invocation::{InvocationOutcome, InvocationReport, StepFailure, StepStatus};

pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(600);
/// Runs of one firing, the first included, before an unavailable one is final.
pub const MAX_ATTEMPTS: u32 = 8;

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
        StepFailure::Host { error } if matches!(**error, ActionCallError::Unavailable { .. }) => {
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

/// How a triggered run ended for good.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    /// It ran to its end.
    Ran,
    /// It was refused: trying again changes nothing.
    Refused,
    /// It failed, or stayed unavailable through all its attempts.
    Failed,
}

/// What follows a run's end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// The firing is done: how its run ended, at which step and why when it did not run to its
    /// end.
    Done {
        ended: Ended,
        step: Option<String>,
        why: Option<String>,
    },
    /// The firing goes again at `at_ms`, ms since the epoch.
    Again { at_ms: i64 },
}

/// What follows `report`, the end of attempt `attempt` (the first is 1), at `now_ms`.
#[must_use]
pub fn after_run(report: &InvocationReport, attempt: u32, now_ms: i64) -> Next {
    let failure = match &report.outcome {
        InvocationOutcome::Completed => {
            return Next::Done {
                ended: Ended::Ran,
                step: None,
                why: None,
            };
        }
        InvocationOutcome::Cancelled { .. } => {
            return Next::Done {
                ended: Ended::Failed,
                step: None,
                why: Some("the run was cancelled".to_owned()),
            };
        }
        InvocationOutcome::Failed { failure, .. } => failure,
    };
    let step = report.steps.iter().rev().find_map(|entry| {
        matches!(entry.status, StepStatus::Failed { .. }).then(|| entry.step.clone())
    });
    after_failure(
        classify(failure) == FailureClass::Transient,
        failure.to_string(),
        step,
        attempt,
        now_ms,
    )
}

/// What follows a run that ended at `step` for `why` on attempt `attempt`, or did not start:
/// another attempt after its backoff while it could go differently and attempts are left, else
/// done.
#[must_use]
pub fn after_failure(
    transient: bool,
    why: String,
    step: Option<String>,
    attempt: u32,
    now_ms: i64,
) -> Next {
    if transient && attempt < MAX_ATTEMPTS {
        let wait = i64::try_from(backoff(attempt).as_millis()).unwrap_or(i64::MAX);
        return Next::Again {
            at_ms: now_ms.saturating_add(wait),
        };
    }
    Next::Done {
        ended: if transient {
            Ended::Failed
        } else {
            Ended::Refused
        },
        step,
        why: Some(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StepPath;
    use crate::invocation::{AutomationInvocationId, InvocationMode};

    fn report(outcome: InvocationOutcome) -> InvocationReport {
        InvocationReport {
            invocation: AutomationInvocationId(uuid::Uuid::from_bytes([1; 16])),
            shortcut: "s".into(),
            version: None,
            trigger: "t".into(),
            mode: InvocationMode::Unattended,
            steps: vec![],
            output: None,
            degraded: Vec::new(),
            outcome,
        }
    }

    fn failed(failure: StepFailure) -> InvocationOutcome {
        InvocationOutcome::Failed {
            path: StepPath::root().child(0),
            failure,
        }
    }

    tilo_test_env::scenario! {
        fn a_run_goes_again_only_while_it_could_go_differently;

        phase a_run_that_ran_is_done;
        {
            assert_eq!(
                after_run(&report(InvocationOutcome::Completed), 1, 0),
                Next::Done { ended: Ended::Ran, step: None, why: None }
            );
        }

        phase an_unavailable_step_goes_again_with_backoff_until_its_attempts_are_spent;
        {
            let unavailable = report(failed(StepFailure::Host {
                error: Box::new(ActionCallError::Unavailable {
                    reason: "down".into(),
                }),
            }));
            let mut now = 1_000;
            for attempt in 1..MAX_ATTEMPTS {
                let Next::Again { at_ms } = after_run(&unavailable, attempt, now) else {
                    panic!("attempt {attempt} goes again");
                };
                assert_eq!(
                    at_ms - now,
                    i64::try_from(backoff(attempt).as_millis()).unwrap()
                );
                now = at_ms;
            }
            let Next::Done { ended, why, .. } = after_run(&unavailable, MAX_ATTEMPTS, now) else {
                panic!("its attempts are spent");
            };
            assert_eq!(ended, Ended::Failed);
            assert!(why.unwrap().contains("down"));
        }

        phase a_refusal_is_done_at_once_with_its_step;
        {
            let outside = || StepFailure::OutsideGrant {
                app_id: "a".into(),
                action: "b".into(),
            };
            let mut denied = report(failed(outside()));
            denied.steps.push(crate::invocation::StepEntry {
                path: StepPath::root().child(0),
                step: "post".into(),
                invocation: None,
                attempts: Vec::new(),
                degraded: crate::StepOutcome::None,
                status: StepStatus::Failed {
                    failure: outside(),
                    attempt: 1,
                    recovered: crate::Recovered::Stopped,
                },
            });
            let Next::Done { ended, step, why } = after_run(&denied, 1, 5) else {
                panic!("done");
            };
            assert_eq!(ended, Ended::Refused);
            assert_eq!(step.as_deref(), Some("post"));
            assert!(why.unwrap().contains("grant"));
        }

        phase backoff_doubles_from_one_second_to_ten_minutes;
        {
            assert_eq!(backoff(1), Duration::from_secs(1));
            assert_eq!(backoff(2), Duration::from_secs(2));
            assert_eq!(backoff(4), Duration::from_secs(8));
            assert_eq!(backoff(20), Duration::from_secs(600));
        }

        phase failures_are_classified_as_the_spec_says;
        {
            let host = |error| StepFailure::Host {
                error: Box::new(error),
            };
            assert_eq!(
                classify(&host(ActionCallError::Unavailable {
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
                classify(&StepFailure::Arguments {
                    reason: String::new()
                }),
                FailureClass::Refusal
            );
            assert_eq!(
                classify(&host(ActionCallError::NotPermitted)),
                FailureClass::Refusal
            );
        }
    }
}
