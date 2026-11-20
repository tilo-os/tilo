//! An app's operations a test makes slow: each one of a workspace's app waits, before its
//! worker is asked, until the test lets it go or a bound passes, as a worker that takes long
//! to answer (a large import, a long effect, one that hangs) does. A test slows one to see
//! what the service answers meanwhile.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

/// One app of one workspace slowed.
struct Slow {
    /// How long each operation waits at most.
    bound: Duration,
    /// How many operations started waiting since it was slowed.
    entered: usize,
}

static SLOWED: Mutex<Option<HashMap<(String, String), Slow>>> = Mutex::new(None);
static LET_GO: Condvar = Condvar::new();

/// The operations of one app of one workspace, slowed until this is dropped.
#[must_use = "the operations are slowed only while this is kept"]
pub struct Slowed {
    app: (String, String),
}

impl Slowed {
    /// Slow every operation of `app` in `workspace`, each for `bound` at most.
    pub fn new(workspace: &str, app: &str, bound: Duration) -> Self {
        let app = (workspace.to_owned(), app.to_owned());
        SLOWED
            .lock_recover()
            .get_or_insert_with(HashMap::new)
            .insert(app.clone(), Slow { bound, entered: 0 });
        Self { app }
    }

    /// How many operations started waiting since it was slowed.
    #[must_use]
    pub fn entered(&self) -> usize {
        SLOWED
            .lock_recover()
            .as_ref()
            .and_then(|slowed| slowed.get(&self.app))
            .map_or(0, |slow| slow.entered)
    }
}

impl Drop for Slowed {
    fn drop(&mut self) {
        if let Some(slowed) = SLOWED.lock_recover().as_mut() {
            slowed.remove(&self.app);
        }
        LET_GO.notify_all();
    }
}

/// Wait while `app` of `workspace` is slowed, its bound at most.
pub(crate) fn wait(workspace: &str, app: &str) {
    let key = (workspace.to_owned(), app.to_owned());
    let mut slowed = SLOWED.lock_recover();
    let Some(slow) = slowed.as_mut().and_then(|slowed| slowed.get_mut(&key)) else {
        return;
    };
    slow.entered = slow.entered.saturating_add(1);
    let until = Instant::now().checked_add(slow.bound);
    while let Some(until) = until {
        let still = slowed
            .as_ref()
            .is_some_and(|slowed| slowed.contains_key(&key));
        let left = until.saturating_duration_since(Instant::now());
        if !still || left.is_zero() {
            break;
        }
        slowed = LET_GO
            .wait_timeout(slowed, left)
            .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
    }
    drop(slowed);
}

/// Whether the service serves `one` and `other` on the same thread: a request of either waits
/// for what the thread does for the other.
#[must_use]
pub fn served_together(one: &str, other: &str) -> bool {
    let count = crate::worker::pool_size();
    match (WorkspaceId::parse(one), WorkspaceId::parse(other)) {
        (Some(one), Some(other)) => {
            super::jobs::shard_of(one, count) == super::jobs::shard_of(other, count)
        }
        _ => false,
    }
}
