//! When the filesystem daemon's endpoint starts its relays.
//!
//! A relay is what makes a device dialable from outside its own network: it
//! holds a home relay connection open with keep-alive pings and, every
//! 20-26 s, re-runs iroh's net report — QUIC address-discovery probes to
//! every relay in the map, DNS lookups and a port-mapping probe. On an idle
//! `fs serve` that nobody could dial, that was 5.8 of its 6.9 wakeups a
//! second (probe run 94694).
//!
//! So `fs serve` binds with an *empty* relay map and holds the default
//! relays here until something means another party may dial this device.
//! iroh skips net report, QAD and port mapping while the map is empty and
//! keeps the relay transport, so [`Reachability::wake`] adds the relays
//! later through the public [`Endpoint::insert_relay`]; the endpoint, its
//! key and its live connections stay as they are. Dialling out works while
//! dormant too: a peer address that names a relay gets a connection to it
//! on demand, and address lookup finds the others.
//!
//! Who can dial a mount? Only a party that knows it, and every way of
//! knowing it leaves a trace the daemon sees. Each one wakes the relays:
//!
//! * a peer the content plane learns of — persisted, bootstrapped from a
//!   ticket, met over gossip or authenticated in a session
//!   ([`crate::Hydrator::add_peer`]);
//! * sync progress persisted with a peer: someone synced with this
//!   namespace before;
//! * a capability list that names anyone other than the mount's own actor,
//!   at start or after any change to it;
//! * a live share grant, or any request to the share control socket;
//! * no capability list at all: an unenforced mount is shared by handing
//!   out its ticket, which leaves no trace.
//!
//! Waking is sticky for the life of the process. Same model as the
//! compositor's (`shell::network::reachability`).

use std::sync::{Arc, Mutex};

use iroh::{Endpoint, RelayConfig, RelayMap};

use crate::acl::AclHandle;

/// The relays an endpoint holds back until something needs it reachable.
/// Cloning shares the handle; [`Self::default`] holds nothing back, so it
/// is always reachable as configured.
#[derive(Clone, Default)]
pub struct Reachability {
    deferred: Arc<Mutex<Option<Deferred>>>,
}

struct Deferred {
    endpoint: Endpoint,
    relays: Vec<Arc<RelayConfig>>,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for Reachability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reachability")
            .field("dormant", &self.is_dormant())
            .finish()
    }
}

impl Reachability {
    /// Hold `relays` back from `endpoint`, which must have been bound with
    /// a relay transport and an empty relay map
    /// (`RelayMode::Custom(RelayMap::empty())`); [`Self::wake`] inserts them
    /// on `runtime`. An empty `relays` holds nothing back.
    #[must_use]
    pub fn defer(endpoint: &Endpoint, relays: &RelayMap, runtime: tokio::runtime::Handle) -> Self {
        let relays: Vec<Arc<RelayConfig>> = relays.relays();
        if relays.is_empty() {
            return Self::default();
        }
        Self {
            deferred: Arc::new(Mutex::new(Some(Deferred {
                endpoint: endpoint.clone(),
                relays,
                runtime,
            }))),
        }
    }

    /// Whether relays are still held back.
    #[must_use]
    pub fn is_dormant(&self) -> bool {
        self.deferred
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// Start the held-back relays, because of `why`. Returns whether this
    /// call started them; later calls (and calls on a handle that never
    /// held any back) do nothing. Safe from any thread.
    pub fn wake(&self, why: &str) -> bool {
        let Some(Deferred {
            endpoint,
            relays,
            runtime,
        }) = self
            .deferred
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return false;
        };
        println!(
            "Starting {} relays: this device may be dialled ({why})",
            relays.len()
        );
        runtime.spawn(async move {
            for relay in relays {
                let url = relay.url.clone();
                endpoint.insert_relay(url, relay).await;
            }
        });
        true
    }

    /// Wake once `acl` names anyone other than the mount's own actor: now,
    /// or after any change to it while dormant. Returns the task watching
    /// for that change, if one was needed.
    pub fn wake_when_acl_admits_others(
        &self,
        acl: &AclHandle,
        what: &'static str,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if !self.is_dormant() {
            return None;
        }
        if acl.snapshot().names_others() {
            self.wake(what);
            return None;
        }
        let mut revisions = acl.subscribe_revision();
        let acl = acl.clone();
        let reachability = self.clone();
        Some(tokio::spawn(async move {
            while revisions.changed().await.is_ok() {
                if !reachability.is_dormant() {
                    return;
                }
                if acl.snapshot().names_others() {
                    reachability.wake(what);
                    return;
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use collab_types::{Access, CapOverride, Ed25519PublicKey, Effect, Principal, Scope, Subject};
    use iroh::endpoint::presets;
    use iroh::{RelayMap, RelayMode, RelayUrl};

    use super::Reachability;
    use crate::acl::{AclHandle, WorkspaceAcl};

    /// Two relays nobody listens on: the test observes the relay map, not
    /// a relay connection.
    fn unused_relays() -> RelayMap {
        ["http://127.0.0.1:9/", "http://127.0.0.1:10/"]
            .into_iter()
            .map(|url| url.parse::<RelayUrl>().unwrap())
            .collect()
    }

    /// An endpoint bound the way `fs serve` binds one, and its relay map:
    /// the map is shared, so it shows what the endpoint's net report and
    /// relay transport see.
    async fn endpoint_without_relays() -> (iroh::Endpoint, RelayMap) {
        let relays = RelayMap::empty();
        let endpoint = iroh::Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Custom(relays.clone()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        (endpoint, relays)
    }

    async fn until_relays(relays: &RelayMap, count: usize) {
        let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while relays.len() != count {
            assert!(
                Instant::now() < deadline,
                "the endpoint's relay map never reached {count} relays"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn grant(subject: Subject) -> CapOverride {
        CapOverride::new(
            subject,
            Scope::FsPath("/".into()),
            Effect::Grant,
            Access::Write,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relays_wait_until_woken_and_wake_once() {
        let (endpoint, relays) = endpoint_without_relays().await;
        let reachability = Reachability::defer(
            &endpoint,
            &unused_relays(),
            tokio::runtime::Handle::current(),
        );
        assert!(reachability.is_dormant());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(relays.len(), 0, "dormant relays are not in the map");
        let shared = reachability.clone();
        assert!(shared.wake("a test"));
        assert!(!reachability.is_dormant(), "clones share one state");
        until_relays(&relays, 2).await;
        assert!(!reachability.wake("again"), "waking is once");
        assert!(!Reachability::default().wake("nothing held"));
        endpoint.close().await;
    }

    /// The shipped capability list grants only the mount's own principal:
    /// nobody else can be admitted, so the relays stay asleep. A grant to
    /// another person, made while the mount runs, wakes them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_grant_to_someone_else_wakes_the_relays() {
        let me = [1; 32];
        let local = Principal::Person("local".into());
        let own = vec![grant(Subject::Principal(local.clone()))];
        let acl = AclHandle::new(WorkspaceAcl::from_capabilities(
            me,
            &own,
            std::slice::from_ref(&local),
            None,
        ));
        let (endpoint, relays) = endpoint_without_relays().await;
        let reachability = Reachability::defer(
            &endpoint,
            &unused_relays(),
            tokio::runtime::Handle::current(),
        );
        let watcher = reachability
            .wake_when_acl_admits_others(&acl, "a test grant")
            .expect("an ACL naming only this mount leaves the relays asleep");
        acl.replace(WorkspaceAcl::from_capabilities(
            me,
            &own,
            std::slice::from_ref(&local),
            None,
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            reachability.is_dormant(),
            "a change that still names only this mount must not wake them"
        );
        let mut shared = own.clone();
        shared.push(grant(Subject::User(Ed25519PublicKey([2; 32]))));
        acl.replace(WorkspaceAcl::from_capabilities(
            me,
            &shared,
            std::slice::from_ref(&local),
            None,
        ));
        until_relays(&relays, 2).await;
        assert!(!reachability.is_dormant());
        tokio::time::timeout(Duration::from_secs(5), watcher)
            .await
            .expect("the watcher ends once the relays are up")
            .unwrap();
        endpoint.close().await;
    }

    /// An ACL that already names someone else wakes the relays at once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_acl_naming_someone_else_wakes_the_relays_at_start() {
        let acl = AclHandle::new(WorkspaceAcl::from_capabilities(
            [1; 32],
            &[grant(Subject::Principal(Principal::Room("team".into())))],
            &[],
            None,
        ));
        let (endpoint, relays) = endpoint_without_relays().await;
        let reachability = Reachability::defer(
            &endpoint,
            &unused_relays(),
            tokio::runtime::Handle::current(),
        );
        assert!(
            reachability
                .wake_when_acl_admits_others(&acl, "a room grant")
                .is_none()
        );
        until_relays(&relays, 2).await;
        endpoint.close().await;
    }
}
