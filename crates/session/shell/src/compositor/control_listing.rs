//! An `actions_enumerate` a control connection answers once its listing
//! settles: `complete` — no index build or app directory page that would
//! change it still owed — and, with `after_revision`, different from the
//! listing the caller already has.
//!
//! Nothing here polls. A held listing runs again only once the action
//! surface moved since it last ran ([`ListingStamp`]) or its wait ran out;
//! every move that matters wakes the loop: the broker's invocation and
//! publication watch, and the index and directory answers that wake it
//! through [`invocation_wake`]. Running again is the same request through
//! the same path, so what it may see is decided exactly as the first time.

use std::time::{Duration, Instant};

use crate::compositor::control_invocation::MAX_SETTLE_WAIT;
use crate::compositor::test_control;
use crate::shell::search::invocation_wake;

/// How soon a listing still owed something is looked at again although
/// nothing woke the loop: an app's directory page that never answers is
/// given up on at its own deadline, which signals nobody.
const RECHECK: Duration = Duration::from_secs(1);

/// What a held listing compares to know whether anything it waits for can
/// have happened since it last ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ListingStamp {
    /// [`invocation_wake::progress`]: index builds, directory pages,
    /// admissions and broker changes the loop was woken for.
    pub(crate) progress: u64,
    /// The broker's invocation and publication generations.
    pub(crate) broker: Option<(u64, u64)>,
}

/// A listing request's wait, as it runs: until when, past which revision,
/// and the action surface as it was before this run read the listing.
pub(crate) struct ListingWait {
    settle_by: Instant,
    after_revision: Option<String>,
    stamp: ListingStamp,
    /// The broker's next change, armed before the listing was read.
    change: Option<tilo_actions::remote::Pending<()>>,
}

/// A listing held back until it settles, kept as the request line it
/// runs again from.
pub(crate) struct HeldListing {
    pub(crate) line: zeroize::Zeroizing<String>,
    pub(crate) settle_by: Instant,
    stamp: ListingStamp,
    change: Option<tilo_actions::remote::Pending<()>>,
    /// The listing was incomplete: something is owed that may never signal.
    owed: bool,
}

impl ListingWait {
    /// The wait `command` asks for: an `actions_enumerate` with `wait_ms`.
    /// A listing run again keeps the deadline of its first run, `held_by`.
    pub(crate) fn begin(
        command: &test_control::Command,
        held_by: Option<Instant>,
        stamp: ListingStamp,
        change: impl FnOnce() -> Option<tilo_actions::remote::Pending<()>>,
    ) -> Option<Self> {
        let test_control::Command::ActionsEnumerate {
            wait_ms: Some(wait_ms),
            after_revision,
            ..
        } = command
        else {
            return None;
        };
        let now = Instant::now();
        let settle_by = held_by.unwrap_or_else(|| {
            now.checked_add(Duration::from_millis(*wait_ms).min(MAX_SETTLE_WAIT))
                .unwrap_or(now)
        });
        Some(Self {
            settle_by,
            after_revision: after_revision.clone(),
            stamp,
            change: change(),
        })
    }

    /// The answer to send, marked with whether it `settled`; or, while it
    /// has not and there is time left, the listing to hold, which runs
    /// again from `line`.
    ///
    /// # Errors
    ///
    /// The listing to hold back on the connection.
    pub(crate) fn settle(
        self,
        response: test_control::Response,
        line: &str,
    ) -> Result<test_control::Response, HeldListing> {
        let test_control::Response::Answer(mut answer) = response else {
            // A refusal is final.
            return Ok(response);
        };
        let complete = answer.get("complete") != Some(&serde_json::Value::Bool(false));
        let moved = self.after_revision.as_deref().is_none_or(|seen| {
            answer.get("revision").and_then(serde_json::Value::as_str) != Some(seen)
        });
        let settled = complete && moved;
        if !settled && Instant::now() < self.settle_by {
            return Err(HeldListing {
                line: zeroize::Zeroizing::new(line.to_string()),
                settle_by: self.settle_by,
                stamp: self.stamp,
                change: self.change,
                owed: !complete,
            });
        }
        if let Some(fields) = answer.as_object_mut() {
            fields.insert("settled".into(), serde_json::Value::Bool(settled));
        }
        Ok(test_control::Response::Answer(answer))
    }
}

impl HeldListing {
    /// Whether to run the listing again: its wait ran out, or the action
    /// surface moved since it last ran. Its broker watch is polled with the
    /// loop's waker, so a change wakes the loop to ask this again.
    pub(crate) fn due(&mut self, now: Instant, stamp: ListingStamp) -> bool {
        if let Some(change) = self.change.as_mut() {
            let waker = invocation_wake::waker();
            if !matches!(
                invocation_wake::poll(change, &waker),
                tilo_actions::remote::PendingPoll::Waiting
            ) {
                return true;
            }
        }
        now >= self.settle_by || stamp != self.stamp
    }

    /// When the loop must look again although nothing woke it.
    pub(crate) fn recheck_at(&self, now: Instant) -> Option<Instant> {
        if self.owed {
            now.checked_add(RECHECK)
                .map_or(Some(self.settle_by), |soon| Some(soon.min(self.settle_by)))
        } else {
            Some(self.settle_by)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(wait_ms: Option<u64>, after: Option<&str>) -> test_control::Command {
        test_control::Command::ActionsEnumerate {
            query: "read".into(),
            include_programmatic: false,
            page: test_control::ListingRequest::default(),
            wait_ms,
            after_revision: after.map(str::to_string),
        }
    }

    fn stamp(progress: u64) -> ListingStamp {
        ListingStamp {
            progress,
            broker: Some((1, 1)),
        }
    }

    fn answer(complete: bool, revision: &str) -> test_control::Response {
        test_control::Response::Answer(
            serde_json::json!({"actions": [], "complete": complete, "revision": revision}),
        )
    }

    fn wait(command: &test_control::Command) -> ListingWait {
        ListingWait::begin(command, None, stamp(0), || None).expect("a waiting listing")
    }

    #[test]
    fn only_a_listing_with_wait_ms_waits_and_its_wait_is_bounded() {
        assert!(ListingWait::begin(&listing(None, None), None, stamp(0), || None).is_none());
        let status = test_control::Command::ActionsStatus {
            invocation_id: 1,
            wait_ms: Some(5),
        };
        assert!(ListingWait::begin(&status, None, stamp(0), || None).is_none());
        let long = wait(&listing(Some(u64::MAX), None));
        assert!(
            long.settle_by <= Instant::now().checked_add(MAX_SETTLE_WAIT).expect("later"),
            "capped like every other wait"
        );
        let held_by = Instant::now();
        let rerun =
            ListingWait::begin(&listing(Some(9_000), None), Some(held_by), stamp(0), || {
                None
            })
            .expect("a rerun");
        assert_eq!(rerun.settle_by, held_by, "a rerun keeps its first deadline");
    }

    #[test]
    fn a_listing_settles_once_complete_and_changed() {
        let Ok(test_control::Response::Answer(done)) =
            wait(&listing(Some(1_000), None)).settle(answer(true, "r1"), "line")
        else {
            panic!("a complete listing answers");
        };
        assert_eq!(done["settled"], true);

        let Err(mut held) = wait(&listing(Some(1_000), None)).settle(answer(false, "r1"), "line")
        else {
            panic!("an incomplete listing is held");
        };
        assert_eq!(held.line.as_str(), "line");
        let now = Instant::now();
        assert!(!held.due(now, stamp(0)), "nothing moved");
        assert!(held.due(now, stamp(1)), "progress moved");
        assert!(
            held.recheck_at(now) <= now.checked_add(RECHECK),
            "something owed may never signal"
        );

        let Err(unchanged) =
            wait(&listing(Some(1_000), Some("r1"))).settle(answer(true, "r1"), "line")
        else {
            panic!("the same listing does not settle a wait past it");
        };
        assert_eq!(
            unchanged.recheck_at(now),
            Some(unchanged.settle_by),
            "a complete listing waits on events alone"
        );
        let Ok(test_control::Response::Answer(changed)) =
            wait(&listing(Some(1_000), Some("r1"))).settle(answer(true, "r2"), "line")
        else {
            panic!("a different listing settles");
        };
        assert_eq!(changed["settled"], true);

        let refusal = test_control::Response::Err {
            code: "enumerate_failed".into(),
            message: "no".into(),
        };
        assert_eq!(
            wait(&listing(Some(1_000), None))
                .settle(refusal.clone(), "line")
                .ok(),
            Some(refusal)
        );
    }

    #[test]
    fn a_listing_whose_wait_ran_out_answers_unsettled() {
        let Ok(test_control::Response::Answer(late)) =
            wait(&listing(Some(0), None)).settle(answer(false, "r1"), "line")
        else {
            panic!("no time left: answer with what there is");
        };
        assert_eq!(late["settled"], false);
        assert_eq!(late["complete"], false);
    }

    #[test]
    fn a_broker_change_makes_a_held_listing_due() {
        let (signal, change) = tilo_actions::remote::pending::<()>();
        let mut held =
            ListingWait::begin(&listing(Some(1_000), None), None, stamp(0), || Some(change))
                .expect("a waiting listing")
                .settle(answer(false, "r1"), "line")
                .expect_err("held");
        assert!(!held.due(Instant::now(), stamp(0)));
        signal.complete(());
        assert!(held.due(Instant::now(), stamp(0)), "the broker moved");
    }
}
