//! Why an app's long-lived connection does not run on this device, and when this device may try
//! it again.
//!
//! A start that fails the same way is retried with a wait that doubles, up to a cap, so a failing
//! app is not started in a loop. A connection that came up and then ended soon after counts as
//! the same failure, with its own last words as the reason: a crash loop backs off too. After
//! [`Retry::identical`] failures in a row it stops, and the app's row says why, in its own words
//! or its launcher's first error lines; so does a start refused for a reason no time mends, such
//! as a runtime the app needs that is not installed. A stopped app is still looked at after the
//! cap, by a start that refuses without starting anything: that is how a reinstall, or a runtime
//! that arrived, is found. A connection that ran for [`Retry::settled`] forgets its failures, and
//! opening the app or stopping its connection forgets them at once.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;
use tilo_workspace_service::work::StartRefusal;

/// Which connection: the workspace, and the app.
type Key = (String, String);

/// How this device retries a connection that failed, whether it never came up or ended soon
/// after it did: the first wait, doubled per failure up to `cap`, how long a run must last
/// before its failures are forgotten, and how many identical failures in a row stop it until
/// something changes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Retry {
    /// How long after the first failure the next start comes.
    pub first: Duration,
    /// The longest a wait grows to, and how often a stopped app is looked at again.
    pub cap: Duration,
    /// How long a connection that came up must run for its failures to be forgotten: one that
    /// ends before this is a failure like any other, so a crash loop backs off too.
    pub settled: Duration,
    /// How many times the same failure in a row stops the app here.
    pub identical: u32,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(5),
            cap: Duration::from_secs(300),
            settled: Duration::from_secs(60),
            identical: 3,
        }
    }
}

impl Retry {
    /// How long to wait after `attempt` failures in a row, when it has not stopped.
    fn wait(self, attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(16);
        self.first.saturating_mul(1_u32 << doublings).min(self.cap)
    }
}

/// One app's connection that did not come up on this device.
struct Failure {
    /// What the last failed start said, as the app or its launcher did.
    reason: String,
    /// How many times in a row this same reason stopped it.
    attempts: u32,
    /// The runtime the app needs and this device does not have, when that is why it stopped.
    missing: Option<String>,
    /// The installed package the start ran, as its payload lives: a reinstall is another one,
    /// which is a change that mends the failure.
    package: String,
    /// When this device may try again: the cap once it stopped, so a change is found.
    at: Instant,
    /// Whether it stopped until something changes.
    stopped: bool,
}

/// The apps whose connections this device stopped starting, by workspace and app.
pub(crate) struct Failures {
    by_key: Mutex<BTreeMap<Key, Failure>>,
    /// Moves when what the rows say of an app changes, so every view of them reads again.
    moved: tokio::sync::watch::Sender<u64>,
}

impl Default for Failures {
    fn default() -> Self {
        Self {
            by_key: Mutex::new(BTreeMap::new()),
            moved: tokio::sync::watch::Sender::new(0),
        }
    }
}

impl Failures {
    /// Why this device may not start `app`'s connection in `workspace` now, when it may not: the
    /// failure's reason and the wait still to go. `None` when the start may run, including when a
    /// change mended the failure, which it forgets.
    ///
    /// `package` is the installed package a start would run, and `runtime_installed` says
    /// whether a runtime the app needed is here now.
    pub(crate) fn waiting(
        &self,
        workspace: &str,
        app: &str,
        package: &str,
        runtime_installed: impl FnOnce(&str) -> bool,
        retry: Retry,
    ) -> Option<StartRefusal> {
        let key = (workspace.to_owned(), app.to_owned());
        let mut known = self.by_key.lock_recover();
        let failure = known.get(&key)?;
        if failure.package != package || failure.missing.as_deref().is_some_and(runtime_installed) {
            known.remove(&key);
            drop(known);
            self.moved();
            return None;
        }
        if !failure.stopped && Instant::now() >= failure.at {
            return None;
        }
        let again = failure.at.saturating_duration_since(Instant::now());
        Some(StartRefusal {
            reason: failure.reason.clone(),
            again: if failure.stopped { retry.cap } else { again },
        })
    }

    /// Record that the start of `app`'s connection in `workspace` failed with `reason`: how long
    /// until the next one, and after enough of the same, that it stopped. `missing` names the
    /// runtime the app needs that is not installed, which stops it at once.
    #[must_use]
    pub(crate) fn failed(
        &self,
        workspace: &str,
        app: &str,
        package: &str,
        reason: String,
        missing: Option<&str>,
        retry: Retry,
    ) -> StartRefusal {
        let key = (workspace.to_owned(), app.to_owned());
        let mut known = self.by_key.lock_recover();
        let failure = known.entry(key).or_insert_with(|| Failure {
            reason: reason.clone(),
            attempts: 0,
            missing: None,
            package: package.to_owned(),
            at: Instant::now(),
            stopped: false,
        });
        let was_stopped = failure.stopped;
        let was_reason = failure.reason.clone();
        failure.attempts = if failure.reason == reason && failure.missing.as_deref() == missing {
            failure.attempts.saturating_add(1)
        } else {
            1
        };
        failure.reason.clone_from(&reason);
        package.clone_into(&mut failure.package);
        failure.missing = missing.map(str::to_owned);
        failure.stopped = missing.is_some() || failure.attempts >= retry.identical;
        let wait = if failure.stopped {
            retry.cap
        } else {
            retry.wait(failure.attempts)
        };
        failure.at = Instant::now()
            .checked_add(wait)
            .unwrap_or_else(Instant::now);
        let stopped = failure.stopped;
        drop(known);
        if stopped && (!was_stopped || was_reason != reason) {
            self.moved();
        }
        StartRefusal {
            reason,
            again: wait,
        }
    }

    /// Forget what this device saw of `app`'s connection in `workspace`: a person opened or
    /// stopped it, and the next turn asks for it again.
    pub(crate) fn forget(&self, workspace: &str, app: &str) {
        let forgotten = self
            .by_key
            .lock_recover()
            .remove(&(workspace.to_owned(), app.to_owned()));
        if forgotten.is_some_and(|failure| failure.stopped) {
            self.moved();
        }
    }

    /// Every app whose connection is stopped here, with why, for the rows that say it: an app
    /// stopped in any workspace reads so on this device.
    #[must_use]
    pub(crate) fn stopped_rows(&self) -> Vec<(String, String)> {
        self.by_key
            .lock_recover()
            .iter()
            .filter(|(_, failure)| failure.stopped)
            .map(|((_, app), failure)| (app.clone(), failure.reason.clone()))
            .collect()
    }

    /// A watch that moves when what a row says of an app changes.
    #[must_use]
    pub(crate) fn watch(&self) -> tokio::sync::watch::Receiver<u64> {
        self.moved.subscribe()
    }

    fn moved(&self) {
        self.moved
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}
