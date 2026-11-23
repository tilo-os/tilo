//! What a mount is doing: the requests it is answering now, and when it last took one.
//!
//! A process that shows the mount and is about to end (stopped, or handing the device over to
//! a newer Tilo) reads this first. A request it ends in the middle of is answered with an I/O
//! error, and a program reading a mapped file through it (git reading its packs) dies of
//! SIGBUS: it waits for the requests under way to be answered, and steps down only once the
//! mount has been quiet for a while.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The requests a mount is answering, and when the last one ended.
#[derive(Debug)]
pub struct Activity {
    epoch: Instant,
    in_flight: AtomicUsize,
    /// When the last request was taken or answered, in nanoseconds since `epoch`.
    last_ns: AtomicU64,
    changed: tokio::sync::Notify,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            in_flight: AtomicUsize::new(0),
            last_ns: AtomicU64::new(0),
            changed: tokio::sync::Notify::new(),
        }
    }
}

impl Activity {
    /// A request taken now: it counts as under way until what this answers is dropped, which a
    /// request answered later (a read hydrating on the runtime) moves along with its reply.
    #[must_use]
    pub fn begin(self: &Arc<Self>) -> Answering {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        self.touch();
        Answering(Arc::clone(self))
    }

    /// How many requests are under way.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    /// How long since the last request was taken or answered.
    pub fn quiet_for(&self) -> Duration {
        let last = Duration::from_nanos(self.last_ns.load(Ordering::Acquire));
        self.epoch.elapsed().saturating_sub(last)
    }

    /// Whether nothing is under way and nothing was asked for `quiet`.
    pub fn quiet(&self, quiet: Duration) -> bool {
        self.in_flight() == 0 && self.quiet_for() >= quiet
    }

    /// Wait until no request is under way, at most `within`: whether none is.
    pub async fn answered(&self, within: Duration) -> bool {
        let waiting = async {
            loop {
                let changed = self.changed.notified();
                if self.in_flight() == 0 {
                    return;
                }
                changed.await;
            }
        };
        tokio::time::timeout(within, waiting).await.is_ok()
    }

    fn touch(&self) {
        let now = u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.last_ns.fetch_max(now, Ordering::AcqRel);
    }
}

/// A request under way, until dropped.
#[derive(Debug)]
pub struct Answering(Arc<Activity>);

impl Drop for Answering {
    fn drop(&mut self) {
        self.0.touch();
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_request_counts_until_it_is_answered() {
        let activity = Arc::new(Activity::default());
        assert!(activity.quiet(Duration::ZERO));
        let answering = activity.begin();
        assert_eq!(activity.in_flight(), 1);
        assert!(!activity.quiet(Duration::ZERO));
        assert!(!activity.answered(Duration::from_millis(10)).await);
        let waiting = {
            let activity = Arc::clone(&activity);
            tokio::spawn(async move { activity.answered(Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(answering);
        assert!(waiting.await.unwrap());
        assert!(activity.quiet(Duration::ZERO));
        assert!(!activity.quiet(Duration::from_secs(60)));
    }
}
