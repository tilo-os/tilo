//! What a Tilo endpoint tells number 0's directory, and when.
//!
//! iroh's `presets::N0` is a relay set and a directory at once. The relays are
//! held back until something needs this device dialable
//! ([`crate::Reachability`]); the directory publishes this
//! device's public key beside its home relay URL to `dns.iroh.link`, which is
//! how a collaborator whose address changed is found again. But nothing in
//! iroh holds a publication back while the relays wait: iroh's publisher
//! filters addresses down to relay URLs and republishes the record it kept
//! every five minutes whatever the endpoint's relay state. A desktop with
//! nobody to talk to therefore still signed in to number 0 every five
//! minutes, for a record nothing would ever ask for.
//!
//! So a Tilo endpoint keeps number 0's relays and its directory, and this
//! crate decides what it tells the directory:
//!
//! * [`collaborating`] — the session's endpoint. It publishes exactly what
//!   `presets::N0` publishes (iroh's own [`PkarrPublisher`], relay URLs only,
//!   on number 0's server) but only while this endpoint has a home relay,
//!   the same condition that wakes the relays. A record handed over while
//!   they rest waits, and goes out with the first relay. Peers are resolved
//!   on number 0's DNS either way: that asks for a collaborator's key, never
//!   this device's.
//! * [`Watchonly`] — an endpoint that publishes nothing and still resolves,
//!   so what its owner opened by key (an invite, a room, another of their own
//!   devices) is still found. For a party that is a client, never a place to
//!   dial: the filesystem's mounts, a probe.
//!
//! Both are `address_lookup` builders an endpoint takes in place of
//! `presets::N0`'s, and both leave the crypto provider and the relay map to
//! the caller.
//!
//! The session's endpoint and the filesystem's are the two a person's device
//! holds, and neither binds the preset: they take [`collaborating`] and
//! [`Watchonly`] with `presets::Minimal`'s crypto provider and number 0's
//! default relay map. `presets::N0` is left to processes that are not a
//! desktop — a relay's diagnostic probe, the initrd's unlock exchange, a
//! headless runtime — each listed with its reason in
//! `tools/policy/check_iroh_presets.py`, which refuses a new one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::address_lookup::{
    AddrFilter, AddressLookup, AddressLookupBuilder, EndpointData, Item, PkarrPublisher,
    PkarrResolver,
};
use iroh::{Endpoint, Watcher as _};
use n0_future::boxed::BoxStream;
use tilo_lock::MutexRecover as _;

/// How often the gate reads the endpoint's home relay, or, in a test that
/// has to watch a gate stay shut, how often it looks. A resting desktop
/// stops: the gate task ends with its endpoint.
#[cfg(not(test))]
const POLL: Duration = Duration::from_secs(5);
#[cfg(test)]
const POLL: Duration = Duration::from_millis(200);

/// The lookups of a session endpoint: number 0's publisher behind the gate,
/// number 0's resolver always.
#[must_use]
pub fn collaborating() -> Gate {
    Gate::default()
}

/// The lookups of an endpoint that is off the directory: no publication, the
/// same lookups of other devices.
#[derive(Clone, Debug, Default)]
pub struct Watchonly;

impl AddressLookupBuilder for Watchonly {
    fn into_address_lookup(
        self,
        endpoint: &Endpoint,
    ) -> Result<impl AddressLookup, iroh::address_lookup::AddressLookupBuilderError> {
        PkarrResolver::n0_dns().into_address_lookup(endpoint)
    }
}

/// The publishing gate: iroh's number 0 publisher and resolver, with the
/// publisher called only while this endpoint has a home relay, and the
/// record the endpoint last handed over published as soon as one arrives.
///
/// Opening is the endpoint's own report of its home relay: it turns true when
/// [`tilo_endpoint_reachability::Reachability`] adds a relay, and false when
/// it takes the last one back. The gate task ends with the endpoint, so a
/// desktop whose endpoint is closed keeps no timer.
#[derive(Clone, Debug, Default)]
pub struct Gate {
    open: Arc<AtomicBool>,
    /// The record the endpoint last handed over, which iroh's publisher
    /// would otherwise republish whether or not there is a relay to name.
    held: Arc<Mutex<Option<EndpointData>>>,
}

impl Gate {
    /// Whether this device may be published right now.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }

    /// Whether a record is waiting for a relay to name.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        self.held.lock_recover().is_some()
    }

    fn open_for(&self, endpoint: &Endpoint, publisher: &dyn AddressLookup) {
        self.open.store(true, Ordering::Relaxed);
        let held = self.held.lock_recover().clone();
        if let Some(data) = held {
            publisher.publish(&data);
        }
        tracing::debug!(id = %endpoint.id(), "publishing this device to number 0's directory");
    }
}

impl AddressLookupBuilder for Gate {
    fn into_address_lookup(
        self,
        endpoint: &Endpoint,
    ) -> Result<impl AddressLookup, iroh::address_lookup::AddressLookupBuilderError> {
        let publisher = PkarrPublisher::n0_dns()
            .addr_filter(AddrFilter::relay_only())
            .into_address_lookup(endpoint)?;
        let resolver = PkarrResolver::n0_dns().into_address_lookup(endpoint)?;
        // The gate task holds the publisher it opens; the endpoint's lookup
        // list only ever sees the switch in front of it.
        let watched = endpoint.clone();
        let gate = self.clone();
        let opening: Arc<Box<dyn AddressLookup>> = Arc::new(Box::new(publisher));
        let waking = opening.clone();
        // The endpoint's own runtime drives the watch, and the task ends
        // with it: a desktop whose endpoint is closed keeps no timer.
        tokio::spawn(async move {
            let mut relays = watched.home_relay_status();
            loop {
                let up = relays
                    .get()
                    .iter()
                    .any(iroh::endpoint::RelayStatus::is_connected);
                match (up, gate.is_open()) {
                    (true, false) => gate.open_for(&watched, waking.as_ref().as_ref()),
                    (false, true) => gate.open.store(false, Ordering::Relaxed),
                    _ => {}
                }
                if watched.is_closed() {
                    return;
                }
                let _ = tokio::time::timeout(POLL, relays.updated()).await;
            }
        });
        Ok(Switched {
            gate: self,
            publisher: opening,
            resolver: Box::new(resolver),
        })
    }
}

/// The lookups a gated endpoint has: number 0's publisher behind the gate,
/// number 0's resolver always.
struct Switched {
    gate: Gate,
    publisher: Arc<Box<dyn AddressLookup>>,
    resolver: Box<dyn AddressLookup>,
}

impl std::fmt::Debug for Switched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Switched")
            .field("open", &self.gate.is_open())
            .finish_non_exhaustive()
    }
}

impl AddressLookup for Switched {
    fn publish(&self, data: &EndpointData) {
        *self.gate.held.lock_recover() = Some(data.clone());
        if self.gate.is_open() {
            self.publisher.publish(data);
        }
    }

    fn resolve(
        &self,
        endpoint_id: iroh::EndpointId,
    ) -> Option<BoxStream<Result<Item, iroh::address_lookup::Error>>> {
        self.resolver.resolve(endpoint_id)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use iroh::address_lookup::{AddressLookup as _, AddressLookupBuilder as _};
    use iroh::endpoint::presets;
    use iroh::{RelayMap, RelayMode, SecretKey};

    use super::{Gate, Watchonly, collaborating};
    use crate::Reachability;

    /// A session endpoint as the shell binds one: number 0's lookups behind
    /// the gate, its relays held back until woken. The relay handed back is
    /// the test's own, a loopback server that answers a home relay
    /// connection the moment the gate lets one through.
    async fn session() -> (Gate, Reachability, iroh_relay::server::Server) {
        let (server, relays) = crate::test_support::local_relay().await;
        let gate = collaborating();
        let endpoint = iroh::Endpoint::builder(presets::Minimal)
            .address_lookup(gate.clone())
            .relay_mode(RelayMode::Custom(RelayMap::empty()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .secret_key(SecretKey::generate())
            .bind()
            .await
            .expect("a session endpoint binds");
        let reachability = Reachability::defer(
            &endpoint,
            &relays,
            tokio::runtime::Handle::current(),
            Some(Duration::from_millis(300)),
        );
        (gate, reachability, server)
    }

    async fn until(what: &str, mut ready: impl FnMut() -> bool) {
        tilo_test_env::within(what, async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }

    /// The decision this module exists for: a desktop with nobody to talk to
    /// does not sign its key in to number 0's directory. iroh hands its
    /// record over as the endpoint comes up, relay or none; the gate holds
    /// it, and a resting desktop stays closed against it. The first home
    /// relay is what opens it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resting_desktop_publishes_nothing_until_a_relay_is_up() {
        let (gate, reachability, _relay) = session().await;
        assert!(reachability.is_dormant(), "the relays wait for something");
        assert!(gate.is_holding(), "iroh's record waits in the gate");
        assert!(!gate.is_open(), "nothing publishes from a resting desktop");
        tokio::time::sleep(super::POLL).await;
        assert!(
            !gate.is_open(),
            "a held record is not a reason to sign the device in"
        );

        reachability.wake("a test");
        until("the gate to open on the first home relay", || {
            gate.is_open()
        })
        .await;
    }

    /// Resting again takes the device off the directory: the gate is the
    /// same condition the relays are, so a desktop left alone stops being
    /// dialable by anyone who did not already hold its address.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rested_desktop_closes_the_gate_again() {
        let (gate, reachability, _relay) = session().await;
        reachability.wake("a test");
        until("the gate to open", || gate.is_open()).await;
        assert!(
            !reachability.is_dormant(),
            "the gate opened on a relay, not before one"
        );
        until("the gate to close with the relays", || !gate.is_open()).await;
        assert!(
            reachability.is_dormant(),
            "the gate closed because the relays rested"
        );
    }

    /// A client is never a place to dial: the lookups the filesystem's
    /// endpoints take publish nothing, and still look a peer up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_watchonly_endpoint_publishes_nothing() {
        let endpoint = iroh::Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Custom(RelayMap::empty()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .secret_key(SecretKey::generate())
            .bind()
            .await
            .expect("a client endpoint binds");
        let lookup = Watchonly
            .into_address_lookup(&endpoint)
            .expect("the client's lookups");
        // A record handed over goes nowhere, and the lookups a client keeps
        // are the ones that find its peers.
        lookup.publish(&iroh::address_lookup::EndpointData::default());
        assert!(
            lookup.resolve(SecretKey::generate().public()).is_some(),
            "a client still looks its peers up"
        );
        endpoint.close().await;
    }
}
