//! A document route has one invocation custodian, independent of window focus.
//! Tokens are inert selection fences, never grants or caller-supplied authority.
use std::collections::HashMap;

use super::{ActionDirectoryRoot, CapnpBrokerState, CrdtRoute};
use tilo_actions::remote::{ActionHostError, CanonicalActionRoute};

pub(super) const PROVIDER_PREFIX: &str = "canonical:";

#[derive(Clone)]
struct Publisher {
    route: CrdtRoute,
    registration: uuid::Uuid,
    worker: uuid::Uuid,
    /// The route's custodian is the live worker whose directory was admitted
    /// first, host-mounted or launched alike: once selected it stays until it
    /// is no longer live, then the next earliest takes over. Republishing
    /// keeps a worker's place; a new worker or route is admitted last.
    ordinal: u64,
}

#[derive(Clone)]
struct Selection {
    peer: u64,
    registration: uuid::Uuid,
    token: uuid::Uuid,
}

#[derive(Default)]
pub(super) struct CanonicalDirectories {
    next_ordinal: u64,
    publishers: HashMap<u64, Publisher>,
    selected: HashMap<(String, String, String), Selection>,
}

impl CanonicalDirectories {
    pub(super) fn stream_replaced(&mut self, peer: u64) {
        // A new stable stream cannot resurrect a token issued for a receiver
        // that has since closed, even if no query observed the closed interval.
        for selected in self
            .selected
            .values_mut()
            .filter(|selected| selected.peer == peer)
        {
            selected.token = uuid::Uuid::new_v4();
        }
    }
    pub(super) fn remove(&mut self, peer: u64) {
        self.publishers.remove(&peer);
        self.selected.retain(|_, selection| selection.peer != peer);
    }
}

fn unavailable() -> ActionHostError {
    ActionHostError::Unavailable {
        reason: "the canonical application publisher changed or is unavailable".into(),
    }
}

fn route_key(route: &CrdtRoute) -> (String, String, String) {
    (
        route.cluster_id.clone(),
        route.app_id.clone(),
        route.record_id.clone(),
    )
}

impl CapnpBrokerState {
    pub(super) fn register_canonical_directory(
        &self,
        peer: u64,
        route: &CrdtRoute,
        registration: uuid::Uuid,
    ) -> Result<(), String> {
        let worker = self
            .host_workers
            .lock()
            .map_err(|_| "worker registry unavailable")?
            .get(peer, route)
            .and_then(|identity| identity.worker_registration.clone());
        // Ordinary browsing remains available to non-private publishers, but
        // they cannot claim custody of a canonical invocation route.
        let Some(worker) = worker else {
            return Ok(());
        };
        if worker.route != *route {
            return Err("directory worker route changed".into());
        }
        let mut directories = self
            .canonical_directories
            .lock()
            .map_err(|_| "canonical directory registry unavailable")?;
        let ordinal = if let Some(previous) = directories
            .publishers
            .get(&peer)
            .filter(|previous| previous.worker == worker.identity && previous.route == *route)
        {
            previous.ordinal
        } else {
            directories.next_ordinal = directories
                .next_ordinal
                .checked_add(1)
                .ok_or("canonical directory admission order exhausted")?;
            directories.next_ordinal
        };
        directories.publishers.insert(
            peer,
            Publisher {
                route: route.clone(),
                registration,
                worker: worker.identity,
                ordinal,
            },
        );
        // Replacing a directory invalidates its old token even at the same
        // revision. The publisher retains its place in admission order.
        for selected in directories
            .selected
            .values_mut()
            .filter(|selected| selected.peer == peer)
        {
            selected.registration = registration;
            selected.token = uuid::Uuid::new_v4();
        }
        drop(directories);
        Ok(())
    }

    fn canonical_publisher_is_live(&self, peer: u64, publisher: &Publisher) -> bool {
        let identity = self
            .host_workers
            .lock()
            .ok()
            .and_then(|workers| workers.get(peer, &publisher.route).cloned());
        let Some(identity) = identity else {
            return false;
        };
        let worker_matches = identity.worker_registration.as_ref().is_some_and(|worker| {
            let same_worker = worker.identity == publisher.worker;
            same_worker && worker.route == publisher.route
        });
        worker_matches
            && self
                .revalidate_admission(&publisher.route, &identity)
                .is_ok()
            && self
                .stable_invocation_subscribers
                .lock()
                .is_ok_and(|streams| streams.contains(&peer))
            && self
                .invocation_subscribers
                .lock()
                .is_ok_and(|streams| streams.get(&peer).is_some_and(|stream| !stream.is_closed()))
            && self.action_directory_roots.lock().is_ok_and(|roots| {
                roots.get(&peer).is_some_and(|root| {
                    root.app_id == publisher.route.app_id
                        && root.cluster_id == publisher.route.cluster_id
                        && root.record_id == publisher.route.record_id
                })
            })
    }

    pub(super) fn canonical_directory(
        &self,
        requested: &CanonicalActionRoute,
    ) -> Result<Option<(ActionDirectoryRoot, String)>, ActionHostError> {
        super::validate_directory_identifier(
            &requested.app_id,
            "canonical app",
            super::DIRECTORY_MAX_IDENTIFIER_BYTES,
            true,
        )
        .map_err(|_| ActionHostError::InvalidRequest {
            reason: "invalid canonical application route".into(),
        })?;
        super::validate_directory_identifier(
            &requested.record_id,
            "canonical record",
            super::DIRECTORY_MAX_IDENTIFIER_BYTES,
            false,
        )
        .map_err(|_| ActionHostError::InvalidRequest {
            reason: "invalid canonical record route".into(),
        })?;
        if uuid::Uuid::parse_str(&requested.room).is_err() {
            return Err(ActionHostError::InvalidRequest {
                reason: "invalid canonical room route".into(),
            });
        }
        let route = CrdtRoute::record(&requested.app_id, &requested.room, &requested.record_id);
        let key = route_key(&route);
        let candidates = self.canonical_candidates(&route)?;
        let mut live = candidates
            .into_iter()
            .filter(|(peer, publisher)| self.canonical_publisher_is_live(*peer, publisher))
            .collect::<Vec<_>>();
        live.sort_by_key(|(_, publisher)| publisher.ordinal);
        let mut directories = self
            .canonical_directories
            .lock()
            .map_err(|_| unavailable())?;
        // A snapshot cannot qualify a concurrent replacement.
        live.retain(|(peer, publisher)| {
            directories
                .publishers
                .get(peer)
                .is_some_and(|current| current.registration == publisher.registration)
        });
        let retained = directories.selected.get(&key).cloned().filter(|selected| {
            live.iter().any(|(peer, publisher)| {
                *peer == selected.peer && publisher.registration == selected.registration
            })
        });
        let Some(selected) = retained.or_else(|| {
            live.first().map(|(peer, publisher)| Selection {
                peer: *peer,
                registration: publisher.registration,
                token: uuid::Uuid::new_v4(),
            })
        }) else {
            directories.selected.remove(&key);
            return Ok(None);
        };
        directories.selected.insert(key, selected.clone());
        drop(directories);
        let root = ActionDirectoryRoot {
            peer_id: selected.peer,
            app_id: route.app_id,
            cluster_id: route.cluster_id,
            record_id: route.record_id,
        };
        Ok(Some((root, format!("{PROVIDER_PREFIX}{}", selected.token))))
    }

    fn canonical_candidates(
        &self,
        route: &CrdtRoute,
    ) -> Result<Vec<(u64, Publisher)>, ActionHostError> {
        // Snapshot before checking actual worker custody. Authority services
        // and stream registries must never be called while this mutex is held.
        let directories = self
            .canonical_directories
            .lock()
            .map_err(|_| unavailable())?;
        let candidates = directories
            .publishers
            .iter()
            .filter(|(_, publisher)| publisher.route == *route)
            .map(|(peer, publisher)| (*peer, publisher.clone()))
            .collect();
        drop(directories);
        Ok(candidates)
    }

    pub(super) fn canonical_token_directory(
        &self,
        token: &str,
    ) -> Result<ActionDirectoryRoot, ActionHostError> {
        let uuid = token
            .strip_prefix(PROVIDER_PREFIX)
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .ok_or_else(unavailable)?;
        let requested = self
            .canonical_directories
            .lock()
            .map_err(|_| unavailable())?
            .selected
            .iter()
            .find(|(_, selected)| selected.token == uuid)
            .map(|((room, app_id, record_id), _)| CanonicalActionRoute {
                room: room.clone(),
                app_id: app_id.clone(),
                record_id: record_id.clone(),
            })
            .ok_or_else(unavailable)?;
        let (root, current) = self
            .canonical_directory(&requested)?
            .ok_or_else(unavailable)?;
        if current != token {
            return Err(unavailable());
        }
        Ok(root)
    }
}
