//! The broker's workers: the connections whose edits it admits as the
//! actions of one app on one route, attributed to that app.
//!
//! A worker is a connection's admitted join on a route of its own. A
//! host-mounted worker gets its registration when the broker allocates its
//! private descriptor (`connect_local_worker*`), bound to that descriptor's
//! one route. A packaged app gets its registration when its connection is
//! authenticated by its launcher, bound to the app the launcher verified:
//! each route of that app it joins is one of its worker routes. From there
//! both are the same worker: admitted, revalidated and retired alike.

use std::collections::HashMap;

use super::{AdmittedJoinIdentity, CapnpBrokerState, CrdtRoute, PrivateWorkerRegistration};

/// Bound on live worker routes: one per route a worker connection holds,
/// removed when that connection drops.
///
/// Measured use (Cloud host): one headless worker per hosted app record, and
/// every record ever opened in the room stays hosted (each "Open Notes"
/// creates a record), plus one registered connection per managed agent. On a
/// device, each open app window holds its own app's routes, usually one. A
/// workspace with 16 agents and a few dozen records per app must fit: the
/// former bound of 32 refused the 33rd record, which failed the whole worker
/// mount and so every agent. Each entry is a small admitted identity (two
/// certificates and a few ids, under 2 KiB), so this keeps memory bounded
/// (well under 1 MiB) while leaving room for real workspaces.
pub(super) const MAX_HOST_WORKERS: usize = 256;

/// The admitted worker joins, by connection and route.
#[derive(Default)]
pub(super) struct HostWorkers(HashMap<u64, HashMap<CrdtRoute, AdmittedJoinIdentity>>);

impl HostWorkers {
    /// The worker `peer` is on `route`, if it is one there.
    pub(super) fn get(&self, peer: u64, route: &CrdtRoute) -> Option<&AdmittedJoinIdentity> {
        self.0.get(&peer)?.get(route)
    }

    /// Every admitted worker join.
    pub(super) fn values(&self) -> impl Iterator<Item = &AdmittedJoinIdentity> {
        self.0.values().flat_map(HashMap::values)
    }

    /// The connection holding `registration`, while it is live.
    pub(super) fn peer_of(&self, registration: &PrivateWorkerRegistration) -> Option<u64> {
        self.0.iter().find_map(|(peer, routes)| {
            routes
                .get(&registration.route)
                .is_some_and(|worker| worker.worker_registration.as_ref() == Some(registration))
                .then_some(*peer)
        })
    }

    /// Whether `peer` is a worker on any route.
    #[cfg(test)]
    pub(super) fn holds(&self, peer: u64) -> bool {
        self.0.contains_key(&peer)
    }

    /// Withdraw every route of a connection that dropped.
    pub(super) fn remove(&mut self, peer: u64) {
        self.0.remove(&peer);
    }

    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    /// Admitted worker routes, across every connection.
    pub(super) fn len(&self) -> usize {
        self.0.values().map(HashMap::len).sum()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    pub(super) fn insert(&mut self, peer: u64, route: CrdtRoute, admitted: AdmittedJoinIdentity) {
        self.0.entry(peer).or_default().insert(route, admitted);
    }

    /// The one worker join of a connection holding a single route.
    #[cfg(test)]
    pub(super) fn only(&self, peer: u64) -> Option<&AdmittedJoinIdentity> {
        let routes = self.0.get(&peer)?;
        assert_eq!(routes.len(), 1, "connection {peer} holds one worker route");
        routes.values().next()
    }
}

impl CapnpBrokerState {
    /// Host-local observation only; this predicate is never a wire proof.
    ///
    /// # Errors
    ///
    /// Returns a message when the host worker registry is unavailable.
    pub fn is_current_host_worker(
        &self,
        peer: u64,
        route: &CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<bool, String> {
        if !self.is_accepting() {
            return Ok(false);
        }
        let admitted = self
            .host_workers
            .lock()
            .map_err(|_| "host worker provenance unavailable")?
            .get(peer, route)
            .cloned();
        let Some(admitted) = admitted else {
            return Ok(false);
        };
        if !admitted
            .host_scope
            .as_ref()
            .is_some_and(|scope| &scope.route == route && &scope.certificate == certificate)
        {
            return Ok(false);
        }
        // The current owner callback may open Store. Never call it under the registry lock.
        if self.revalidate_admission(route, &admitted).is_err() {
            return Ok(false);
        }
        let workers = self
            .host_workers
            .lock()
            .map_err(|_| "host worker provenance unavailable")?;
        let unchanged = workers.get(peer, route) == Some(&admitted);
        drop(workers);
        Ok(self.is_accepting() && unchanged)
    }

    /// Keep `peer`'s admitted join on `route` while its connection lives,
    /// when it is a worker there.
    pub(super) fn retain_host_worker(
        &self,
        peer: u64,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<(), capnp::Error> {
        if admitted.host_scope.is_none() && admitted.worker_registration.is_none() {
            return Ok(());
        }
        if !self.is_accepting() {
            return Err(capnp::Error::failed("broker unavailable".into()));
        }
        let mut workers = self
            .host_workers
            .lock()
            .map_err(|_| capnp::Error::failed("host worker provenance unavailable".into()))?;
        if let Some(previous) = workers.get(peer, route) {
            if previous.host_scope != admitted.host_scope
                || previous.worker_registration != admitted.worker_registration
            {
                return Err(capnp::Error::failed(
                    "host worker scope replacement rejected".into(),
                ));
            }
        } else {
            if workers.len() >= MAX_HOST_WORKERS {
                return Err(capnp::Error::failed(
                    "host worker connection capacity reached".into(),
                ));
            }
            workers
                .0
                .entry(peer)
                .or_default()
                .insert(route.clone(), admitted.clone());
        }
        drop(workers);
        Ok(())
    }
}

/// The registration a packaged app's connection holds on a route it joins:
/// its own when the route is its app's, and the join is its person's.
/// `worker` is minted when the launcher authenticated the connection, for
/// the app id the launcher verified.
pub(super) fn packaged_app_registration(
    worker: Option<&(uuid::Uuid, String)>,
    route: &CrdtRoute,
    admitted: &AdmittedJoinIdentity,
) -> Option<PrivateWorkerRegistration> {
    let (identity, app_id) = worker?;
    if route.app_id.is_empty() || &route.app_id != app_id || admitted.agent_id.is_some() {
        return None;
    }
    Some(PrivateWorkerRegistration {
        identity: *identity,
        route: route.clone(),
        certificate: admitted.certificate.clone()?,
    })
}
