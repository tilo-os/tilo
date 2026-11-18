//! A saved tab's Windows program: what the session, as itself, asks the service.
//!
//! The Windows Apps manager reaches the service as one of the session's launcher
//! processes, through the handoff (`crate::session`). The session reopening a tab
//! the person left open is no such launch: it is the service's own session, so it
//! asks here instead. What it asks for is checked exactly as a manager's launch is
//! checked: the entry must be one the service can open now, and the workspace must
//! be one this session serves, whose sandbox the program is granted.
//!
//! The session also reads which entries can be opened now, so a tab whose program
//! was removed says so instead of waiting for a window that never comes.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

fn held<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The entries the service can open now: the programs set up in capsules and the
/// appliance's published applications, as the service last read them. `None` until
/// it read them, and the session waits for that rather than taking a saved tab for
/// one whose program is gone.
#[derive(Clone, Default)]
pub struct EntryDirectory(Arc<Mutex<Option<BTreeSet<String>>>>);

impl EntryDirectory {
    /// The service can open exactly `entries` now.
    pub fn replace(&self, entries: BTreeSet<String>) {
        *held(&self.0) = Some(entries);
    }

    /// The entries the service can open now, or `None` while it has not read them.
    #[must_use]
    pub fn entries(&self) -> Option<BTreeSet<String>> {
        held(&self.0).clone()
    }
}

/// What the session asks the service: the Windows program `entry_id` of a saved
/// tab, opened in `workspace_id`.
pub struct ReopenAsk {
    pub entry_id: String,
    pub workspace_id: String,
    pub answer: ReopenAnswer,
}

/// What became of a [`ReopenAsk`]: given by the service, read by the session's loop
/// on a later pass, which is also woken for it.
#[derive(Clone, Default)]
pub struct ReopenAnswer(Arc<Answer>);

#[derive(Default)]
struct Answer {
    outcome: Mutex<Option<Result<(), String>>>,
    came: Condvar,
    wake: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl ReopenAnswer {
    /// The program was opened, or why it was not.
    pub fn send(&self, outcome: Result<(), String>) {
        let given = {
            let mut outcome_held = held(&self.0.outcome);
            let empty = outcome_held.is_none();
            if empty {
                *outcome_held = Some(outcome);
            }
            empty
        };
        if given {
            self.0.came.notify_all();
            let wake = held(&self.0.wake).take();
            if let Some(wake) = wake {
                wake();
            }
        }
    }

    /// The outcome, once the service gave one. Never waits.
    #[must_use]
    pub fn take(&self) -> Option<Result<(), String>> {
        held(&self.0.outcome).take()
    }

    /// `wake` is called when the outcome is given, so the session's loop reads it
    /// without waiting for the next frame.
    pub fn waking(&self, wake: impl Fn() + Send + Sync + 'static) {
        *held(&self.0.wake) = Some(Box::new(wake));
    }
}

/// The asks the session made since the service last took them.
#[derive(Clone, Default)]
pub struct Reopenings(Arc<Waiting>);

#[derive(Default)]
struct Waiting {
    asks: Mutex<Vec<ReopenAsk>>,
    came: Condvar,
    ended: AtomicBool,
}

impl Reopenings {
    /// Ask the service to open `ask`, and to answer it when it is done.
    pub fn ask(&self, ask: ReopenAsk) {
        held(&self.0.asks).push(ask);
        self.0.came.notify_one();
    }

    /// The next ask, waiting for one; `None` once the service is going away.
    pub fn take(&self) -> Option<ReopenAsk> {
        let mut asks = held(&self.0.asks);
        loop {
            if let Some(ask) = asks.pop() {
                return Some(ask);
            }
            if self.0.ended.load(Ordering::Acquire) {
                return None;
            }
            asks = self
                .0
                .came
                .wait(asks)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// The service is going away: no more asks are taken.
    pub fn stop(&self) {
        self.0.ended.store(true, Ordering::Release);
        self.0.came.notify_all();
    }
}
