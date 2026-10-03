//! The partner watch signal: a renewable lease on a headless compositor's
//! painting.
//!
//! A dashboard showing this machine renews the lease while it is visible.
//! `false` or expiry ends it, so a closed tab or a crashed dashboard cannot
//! leave rendering on. Watching never gates document delivery, agent reads or
//! screenshots, which paint on demand.

use std::{sync::Arc, time::Duration};

/// How long one `watching: true` keeps painting on without a renewal.
pub const WATCH_LEASE: Duration = Duration::from_secs(60);

/// Where the watch state is applied: the compositor a lease drives.
pub trait WatchSink: Send + Sync {
    /// Turn painting for a viewer on or off. Idempotent.
    ///
    /// # Errors
    ///
    /// The compositor could not be reached or refused the signal.
    fn set_watching(&self, watching: bool) -> Result<(), String>;
}

/// One compositor's watch lease, shared by every caller of the endpoint.
pub struct WatchLease {
    sink: Arc<dyn WatchSink>,
    lease: Duration,
    state: tokio::sync::Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Bumped by every signal, so an expiry timer that lost a race with a
    /// renewal cannot end the renewed lease.
    generation: u64,
    expiry: Option<tokio::task::AbortHandle>,
}

impl WatchLease {
    #[must_use]
    pub fn new(sink: Arc<dyn WatchSink>, lease: Duration) -> Arc<Self> {
        Arc::new(Self {
            sink,
            lease,
            state: tokio::sync::Mutex::new(State::default()),
        })
    }

    #[must_use]
    pub const fn lease(&self) -> Duration {
        self.lease
    }

    /// Start or renew the lease (`true`), or end it (`false`). Signals reach
    /// the compositor in the order they were applied.
    ///
    /// # Errors
    ///
    /// The compositor did not take the signal; no lease is then held.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the lock is held across the signal so signals reach the compositor in order"
    )]
    pub async fn apply(self: &Arc<Self>, watching: bool) -> Result<(), String> {
        let mut state = self.state.lock().await;
        state.generation = state.generation.wrapping_add(1);
        if let Some(expiry) = state.expiry.take() {
            expiry.abort();
        }
        self.push(watching).await?;
        if watching {
            let lease = Arc::clone(self);
            let generation = state.generation;
            let timer = tokio::spawn(async move {
                tokio::time::sleep(lease.lease).await;
                lease.expire(generation).await;
            });
            state.expiry = Some(timer.abort_handle());
        }
        Ok(())
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the lock is held across the signal so a renewal cannot overtake it"
    )]
    async fn expire(&self, generation: u64) {
        let mut state = self.state.lock().await;
        if state.generation != generation {
            return;
        }
        state.expiry = None;
        if let Err(error) = self.push(false).await {
            tracing::warn!(%error, "the expired watch lease could not stop painting");
        }
    }

    async fn push(&self, watching: bool) -> Result<(), String> {
        let sink = Arc::clone(&self.sink);
        tokio::task::spawn_blocking(move || sink.set_watching(watching))
            .await
            .map_err(|error| error.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        signals: Mutex<Vec<bool>>,
        refuse: bool,
    }

    impl WatchSink for Recorder {
        fn set_watching(&self, watching: bool) -> Result<(), String> {
            if self.refuse {
                return Err("compositor gone".into());
            }
            self.signals.lock().unwrap().push(watching);
            Ok(())
        }
    }

    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn an_unrenewed_lease_expires_and_stops_painting() {
        let sink = Arc::new(Recorder::default());
        let lease = WatchLease::new(sink.clone(), Duration::from_secs(60));
        run(async {
            lease.apply(true).await.unwrap();
            tokio::time::sleep(Duration::from_secs(59)).await;
            assert_eq!(*sink.signals.lock().unwrap(), [true]);
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        assert_eq!(*sink.signals.lock().unwrap(), [true, false]);
    }

    #[test]
    fn renewals_extend_the_lease() {
        let sink = Arc::new(Recorder::default());
        let lease = WatchLease::new(sink.clone(), Duration::from_secs(60));
        run(async {
            lease.apply(true).await.unwrap();
            tokio::time::sleep(Duration::from_secs(40)).await;
            lease.apply(true).await.unwrap();
            tokio::time::sleep(Duration::from_secs(40)).await;
            assert_eq!(
                *sink.signals.lock().unwrap(),
                [true, true],
                "the renewal outlives the first lease"
            );
            tokio::time::sleep(Duration::from_secs(21)).await;
        });
        assert_eq!(*sink.signals.lock().unwrap(), [true, true, false]);
    }

    #[test]
    fn unwatching_ends_the_lease_at_once_and_only_once() {
        let sink = Arc::new(Recorder::default());
        let lease = WatchLease::new(sink.clone(), Duration::from_secs(60));
        run(async {
            lease.apply(true).await.unwrap();
            lease.apply(false).await.unwrap();
            tokio::time::sleep(Duration::from_secs(120)).await;
        });
        assert_eq!(
            *sink.signals.lock().unwrap(),
            [true, false],
            "no expiry follows an explicit unwatch"
        );
    }

    #[test]
    fn a_refused_signal_holds_no_lease() {
        let sink = Arc::new(Recorder {
            refuse: true,
            ..Recorder::default()
        });
        let lease = WatchLease::new(sink, Duration::from_secs(60));
        run(async {
            assert_eq!(lease.apply(true).await, Err("compositor gone".into()));
            assert!(lease.state.lock().await.expiry.is_none());
        });
    }
}
