//! A connection's holds on the routes it joined.
//!
//! An app window holds every record it has open on one connection: a Chat
//! window its conversations, a Notes window the pages it shows. Each route
//! it joins is one hold, and a hold is what publishes and works there: its
//! actions, its action directory, its canonical publisher entry, its
//! invocation stream and its worker registration are all keyed by the
//! hold's peer id. Joining the same route again shares the hold. When the
//! window drops the last channel on a route, that hold and everything it
//! published leave; when the connection ends, all of its holds leave with
//! it. The connection's own transport identity (document fanout, presence,
//! media, launch) stays the connection's.
//!
//! The first route a connection holds uses the connection's own peer id, so
//! a window with one record is addressed exactly as before; every further
//! route gets a fresh broker-issued peer id from [`FURTHER_HOLDS`] upward,
//! a range accepted connections never reach.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use super::{CapnpBrokerState, CrdtRoute, LocalActionDirectoryRegistry};

/// The first peer id of further route holds. Connection ids count up from
/// one and stay far below it; it stays exact in JSON numbers.
const FURTHER_HOLDS: u64 = 1 << 48;

/// One route a connection holds; dropping the last channel on it withdraws
/// what it published.
pub(super) struct RouteHold {
    id: u64,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
}

impl RouteHold {
    /// The peer id this hold publishes and works as.
    pub(super) const fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for RouteHold {
    fn drop(&mut self) {
        self.state.withdraw_route_hold(self.id);
        // Released after the broker state: dropping a directory capability
        // may re-enter cleanup, which then finds nothing left to withdraw.
        self.directories.remove(self.id);
    }
}

/// The holds of one connection, by route. Owned by the connection's
/// service and so dropped with it.
#[derive(Default)]
pub(super) struct ConnectionHolds(RefCell<HashMap<CrdtRoute, Weak<RouteHold>>>);

impl ConnectionHolds {
    /// The connection's hold on `route`, shared with every live channel on
    /// it, or a new one.
    pub(super) fn hold(
        &self,
        state: &Arc<CapnpBrokerState>,
        directories: &Rc<LocalActionDirectoryRegistry>,
        connection: u64,
        route: &CrdtRoute,
    ) -> Rc<RouteHold> {
        let mut holds = self.0.borrow_mut();
        holds.retain(|_, hold| hold.strong_count() > 0);
        if let Some(hold) = holds.get(route).and_then(Weak::upgrade) {
            return hold;
        }
        let connection_free = !holds
            .values()
            .filter_map(Weak::upgrade)
            .any(|hold| hold.id == connection);
        let id = if connection_free {
            connection
        } else {
            state.hold_route(connection)
        };
        let hold = Rc::new(RouteHold {
            id,
            state: Arc::clone(state),
            directories: Rc::clone(directories),
        });
        holds.insert(route.clone(), Rc::downgrade(&hold));
        drop(holds);
        hold
    }
}

/// Which connection each further route hold belongs to. A connection's own
/// peer id is its first hold and is not listed.
#[derive(Default)]
pub(super) struct RouteHolders {
    holders: HashMap<u64, u64>,
    issued: u64,
}

impl RouteHolders {
    pub(super) fn clear(&mut self) {
        self.holders.clear();
    }
}

impl CapnpBrokerState {
    /// A fresh peer id for a further hold of `connection`.
    fn hold_route(&self, connection: u64) -> u64 {
        let mut holders = self
            .route_holders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        holders.issued = holders.issued.wrapping_add(1);
        let id = FURTHER_HOLDS.saturating_add(holders.issued);
        holders.holders.insert(id, connection);
        drop(holders);
        id
    }

    /// The connection a peer id belongs to: itself unless it is one of a
    /// connection's further route holds.
    pub(super) fn connection_of(&self, peer: u64) -> u64 {
        self.route_holders
            .lock()
            .ok()
            .and_then(|holders| holders.holders.get(&peer).copied())
            .unwrap_or(peer)
    }

    /// Every hold of `connection`, its own peer id first.
    pub(super) fn connection_holds(&self, connection: u64) -> Vec<u64> {
        let mut holds = vec![connection];
        if let Ok(holders) = self.route_holders.lock() {
            holds.extend(
                holders
                    .holders
                    .iter()
                    .filter(|(_, owner)| **owner == connection)
                    .map(|(hold, _)| *hold),
            );
        }
        holds
    }

    /// Forget a further hold once it has been withdrawn.
    pub(super) fn release_route_hold(&self, id: u64) {
        match self.route_holders.lock() {
            Ok(mut holders) => holders.holders.remove(&id),
            Err(poisoned) => poisoned.into_inner().holders.remove(&id),
        };
    }
}
