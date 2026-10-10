//! When this device's endpoint starts its relays.
//!
//! A relay is what makes a device dialable from outside its own network: it
//! holds a home relay connection open (with keep-alive pings) and, every
//! 20-26 s, re-runs iroh's net report, which opens fresh QUIC
//! address-discovery connections to every relay in its map and probes for a
//! port mapping (TIL-702, TIL-740). An idle desktop that nobody could dial
//! pays all of that for nothing: on the installed desktop it was two thirds
//! of the network runtime's idle wakeups.
//!
//! So the endpoint binds with an *empty* relay map and the relays wait here
//! until something needs this device reachable. iroh skips net report, QAD
//! and port mapping while the map is empty, and keeps the relay transport,
//! so [`Reachability::wake`] can add them later through the public
//! [`Endpoint::insert_relay`]: the endpoint, its key and its live
//! connections stay as they are. Outbound dials work while dormant too: a
//! peer address that names a relay gets an on-demand connection to it.
//!
//! Who can dial in? Only a party that already knows this device: a member
//! of a room it is in, someone holding an invite it minted, one of the
//! person's own devices (continuity, the device fabric, disk unlock), a
//! Messages contact, a host. Each of those relationships wakes the relays,
//! at session start when it is already recorded
//! ([`crate::shell::network`]'s `needs_reachability`) or when it begins (the
//! actor's dials, a peer reaching a topic, an invite, a bound Messages
//! account). Waking is sticky for the session.

use std::sync::{Arc, Mutex};

use iroh::{Endpoint, RelayConfig, RelayMap};

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
            .map(|deferred| deferred.is_some())
            .unwrap_or(false)
    }

    /// Start the held-back relays, because of `why`. Returns whether this
    /// call started them; later calls (and calls on a handle that never
    /// held any back) do nothing. Safe from any thread.
    pub fn wake(&self, why: &'static str) -> bool {
        let deferred = match self.deferred.lock() {
            Ok(mut deferred) => deferred.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(Deferred {
            endpoint,
            relays,
            runtime,
        }) = deferred
        else {
            return false;
        };
        tracing::info!(
            why,
            relays = relays.len(),
            "peer-sync: this device needs to be reachable; starting its relays"
        );
        runtime.spawn(insert_relays(endpoint, relays));
        true
    }
}

async fn insert_relays(endpoint: Endpoint, relays: Vec<Arc<RelayConfig>>) {
    for relay in relays {
        let url = relay.url.clone();
        endpoint.insert_relay(url, relay).await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::time::{Duration, Instant};

    use iroh::endpoint::presets;
    use iroh::{RelayMap, RelayMode, RelayUrl};

    use super::Reachability;

    /// Two relays nobody listens on: the test observes the relay map, not
    /// a relay connection.
    fn unused_relays() -> RelayMap {
        RelayMap::from_iter(
            ["http://127.0.0.1:9/", "http://127.0.0.1:10/"]
                .into_iter()
                .map(|url| url.parse::<RelayUrl>().unwrap()),
        )
    }

    /// An endpoint bound the way the shell binds one, and its relay map:
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
        let deadline = Instant::now() + Duration::from_secs(10);
        while relays.len() != count {
            assert!(
                Instant::now() < deadline,
                "the endpoint's relay map never reached {count} relays"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
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
        assert!(
            relays.is_empty(),
            "a dormant endpoint has no relay to probe"
        );

        let shared = reachability.clone();
        assert!(shared.wake("a test"));
        assert!(!reachability.is_dormant(), "clones share one state");
        until_relays(&relays, 2).await;
        assert_eq!(relays.urls::<Vec<_>>(), unused_relays().urls::<Vec<_>>());

        assert!(!reachability.wake("again"), "waking is once per session");
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nothing_to_defer_is_already_reachable() {
        let (endpoint, _) = endpoint_without_relays().await;
        let reachability = Reachability::defer(
            &endpoint,
            &RelayMap::empty(),
            tokio::runtime::Handle::current(),
        );
        assert!(!reachability.is_dormant());
        assert!(!reachability.wake("a test"));
        assert!(!Reachability::default().is_dormant());
        endpoint.close().await;
    }
}
