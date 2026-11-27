//! What the device holds of its workspace as a whole, asked through this device's session: the
//! feed of its records' changes ([`RemoteActionMessage::ChangesRequest`]). The device answers it
//! from what it holds, as it answers its own callers. Its search (`tilo.search#find`) is one of
//! the actions a session answers itself, so it crosses the link as any other call does.

use std::collections::HashMap;

use tilo_actions::remote::{
    ActionCallError, Caller, ChangeFeed, ChangePage, Pending, RemoteActionMessage, pending,
};
use tilo_lock::MutexRecover as _;

use super::{Asked, SessionActions, Waiting};

/// The device's change feed, asked through the session.
pub(super) struct SessionChanges(pub(super) SessionActions);

impl ChangeFeed for SessionChanges {
    fn since(
        &self,
        _caller: &Caller,
        after: Option<&str>,
    ) -> Pending<Result<ChangePage, ActionCallError>> {
        let after = after.map(str::to_owned);
        self.0.ask(
            |waiting| &mut waiting.changes,
            move |correlation| RemoteActionMessage::ChangesRequest { correlation, after },
        )
    }
}

/// Where the answers to one kind of request wait, by correlation.
type Asks<T> = HashMap<u64, Asked<Result<T, ActionCallError>>>;

impl SessionActions {
    /// Send the request `message` makes of a fresh correlation, its answer awaited in the map
    /// `asks` picks: a session that cannot carry it now asks it again once the connection that
    /// is the device's does (`Inner::ask`), and a failure it cannot ask again is answered at
    /// once.
    pub(super) fn ask<T: Send + 'static>(
        &self,
        asks: fn(&mut Waiting) -> &mut Asks<T>,
        message: impl FnOnce(u64) -> RemoteActionMessage,
    ) -> Pending<Result<T, ActionCallError>> {
        let correlation = self
            .0
            .next_correlation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (answer, answered) = pending();
        let message = message(correlation);
        asks(&mut self.0.waiting.lock_recover())
            .insert(correlation, Asked::new(answer, message.clone()));
        self.0.ask(message, |error| {
            let asked = asks(&mut self.0.waiting.lock_recover()).remove(&correlation);
            if let Some(asked) = asked {
                asked.answer.complete(Err(error));
            }
        });
        answered
    }
}
