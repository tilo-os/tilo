//! Wake-up after a Store commit barrier.
//!
//! Held event polls and the private transition observer are resumed by the
//! HTTPS service loop. A commit the loop makes itself (uploads, the private
//! broker's reply) wakes it through that work's own completion, but a commit
//! from another component of this process (an agent worker, retention, an
//! enrollment step) was only observed on the loop's idle tick (up to 250 ms
//! later). Every successful Store publication signals here, after its atomic
//! barrier, so waiting members learn of it at once. The signal carries no
//! state: a woken loop re-reads the durable aggregate as before, and a
//! spurious wake only costs one cheap probe.
use std::sync::LazyLock;
use tokio::sync::watch;

static COMMITS: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);

/// Called after a Store publication passed its durability barrier.
pub(crate) fn committed() {
    COMMITS.send_modify(|count| *count = count.wrapping_add(1));
}

/// A subscription that observes every later commit; a commit made while the
/// holder was busy is kept as a pending change (no lost wake-up).
pub(crate) struct CommitWake(watch::Receiver<u64>);

impl CommitWake {
    pub(crate) fn subscribe() -> Self {
        let mut receiver = COMMITS.subscribe();
        receiver.mark_unchanged();
        Self(receiver)
    }
    /// Resolves once a commit happened since the last call (or subscription).
    pub(crate) async fn changed(&mut self) {
        if self.0.changed().await.is_err() {
            // The sender is a process-wide static and never drops.
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_commit_wakes_a_waiting_subscriber_at_once() {
        let mut wake = CommitWake::subscribe();
        let waiter = tokio::spawn(async move {
            let began = std::time::Instant::now();
            wake.changed().await;
            began.elapsed()
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        committed();
        let waited = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("woken by the commit")
            .unwrap();
        assert!(waited < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_commit_while_busy_is_kept_until_the_next_wait() {
        let mut wake = CommitWake::subscribe();
        committed();
        tokio::time::timeout(Duration::from_millis(200), wake.changed())
            .await
            .expect("a commit made before waiting is not lost");
    }
}
