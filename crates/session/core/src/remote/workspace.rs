//! What the device holds of its workspace as a whole, asked through this device's session: its
//! search ([`RemoteActionMessage::SearchRequest`]) and the feed of its records' changes
//! ([`RemoteActionMessage::ChangesRequest`]). The device answers both from what it holds, as it
//! answers its own callers.

use std::collections::HashMap;

use tilo_actions::remote::{
    ActionHostError, Caller, ChangeFeed, ChangePage, Found, Pending, RemoteActionMessage,
    ResourceSearch, pending,
};
use tilo_lock::MutexRecover as _;

use super::{Asked, SessionActions, Waiting};

/// The device's search, asked through the session.
pub(super) struct SessionSearch(pub(super) SessionActions);

/// The device's change feed, asked through the session.
pub(super) struct SessionChanges(pub(super) SessionActions);

impl ResourceSearch for SessionSearch {
    fn find(&self, _caller: &Caller, needle: &str) -> Pending<Result<Vec<Found>, ActionHostError>> {
        let needle = needle.to_owned();
        self.0.ask(
            |waiting| &mut waiting.searches,
            move |correlation| RemoteActionMessage::SearchRequest {
                correlation,
                needle,
            },
        )
    }
}

impl ChangeFeed for SessionChanges {
    fn since(
        &self,
        _caller: &Caller,
        after: Option<&str>,
    ) -> Pending<Result<ChangePage, ActionHostError>> {
        let after = after.map(str::to_owned);
        self.0.ask(
            |waiting| &mut waiting.changes,
            move |correlation| RemoteActionMessage::ChangesRequest { correlation, after },
        )
    }
}

/// Where the answers to one kind of request wait, by correlation.
type Asks<T> = HashMap<u64, Asked<Result<T, ActionHostError>>>;

impl SessionActions {
    /// Send the request `message` makes of a fresh correlation, its answer awaited in the map
    /// `asks` picks: refused at once when the session cannot carry it.
    fn ask<T: Send + 'static>(
        &self,
        asks: fn(&mut Waiting) -> &mut Asks<T>,
        message: impl FnOnce(u64) -> RemoteActionMessage,
    ) -> Pending<Result<T, ActionHostError>> {
        let correlation = self
            .0
            .next_correlation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (answer, answered) = pending();
        asks(&mut self.0.waiting.lock_recover()).insert(correlation, Asked::new(answer));
        if let Err(error) = self.0.send(message(correlation)) {
            let asked = asks(&mut self.0.waiting.lock_recover()).remove(&correlation);
            if let Some(asked) = asked {
                asked.answer.complete(Err(error));
            }
        }
        answered
    }
}
