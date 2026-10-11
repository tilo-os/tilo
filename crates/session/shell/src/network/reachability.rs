//! When this device's endpoint runs its relays.
//!
//! A relay is what makes a device dialable from outside its own network: it
//! holds a home relay connection open (with keep-alive pings) and, every
//! 20-26 s, re-runs iroh's net report, which opens fresh QUIC
//! address-discovery connections to every relay in its map and probes for a
//! port mapping. An idle desktop that nobody could dial pays all of that for
//! nothing: on the installed desktop it was two thirds of the network
//! runtime's idle wakeups.
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
//! Messages contact, a host. Each of those relationships starts the relays
//! when it begins ([`Reachability::wake`]: a dial, a peer reaching a topic,
//! an invite, the invite composer opening) and holds them up for as long as
//! it lasts ([`Reachability::need`]): the network actor reports the remote
//! parties it is connected to or keeps dialling, the session what it has
//! recorded (a shared room, an open invitation, another of the person's
//! devices).
//!
//! When nothing needs them any more, the relays rest again, [`REST_AFTER`]
//! after the last thing that did, through [`Endpoint::remove_relay`]: with
//! the map empty, net report, QAD and port mapping stop, and the vendored
//! iroh gives up its home relay (`vendor/iroh/TILO-PATCH.md`): the address
//! stops naming it, and the connection and its keep-alive close once it has
//! been idle a minute. The next wake picks a home relay afresh.

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use iroh::{Endpoint, EndpointAddr, RelayConfig, RelayMap};

/// How long the relays stay up after the last thing that needed them: a
/// collaborator who steps away and comes back within it, or an invite
/// composer opened twice, does not pay for a new net report and home relay.
pub const REST_AFTER: Duration = Duration::from_secs(5 * 60);

/// What holds the relays up while it lasts, each reported by the part of
/// the session that knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// A remote party the network actor is connected to or keeps dialling:
    /// a peer, a host, a governed room's relay, one of the person's devices,
    /// a streamed window, a Messages account.
    Transport,
    /// What the session has recorded: a room shared with someone or joined,
    /// an open invitation, another of the person's devices.
    Session,
}

/// The relays an endpoint holds back until something needs it reachable,
/// and holds up while something does. Cloning shares the handle;
/// [`Self::default`] holds nothing back, so it is always reachable as
/// configured.
#[derive(Clone, Default)]
pub struct Reachability {
    held: Cell,
}

type Cell = Arc<Mutex<Option<Held>>>;

struct Held {
    /// This state's own cell, for the tasks it spawns.
    cell: Weak<Mutex<Option<Self>>>,
    endpoint: Endpoint,
    relays: Vec<Arc<RelayConfig>>,
    runtime: tokio::runtime::Handle,
    rest_after: Duration,
    /// Whether the relays should be in the endpoint's map.
    awake: bool,
    /// What holds them up now, by [`Need`].
    needs: Needs,
    /// When something last needed them: a wake, or a need that ended.
    needed_at: Instant,
    /// Whether a task is waiting to rest them.
    rest_pending: bool,
    /// Whether they are in the map, as the last settle left it. Settling
    /// holds this lock, so inserts and removals land in order.
    applied: Arc<tokio::sync::Mutex<bool>>,
}

/// What holds the relays up now, by [`Need`].
#[derive(Default)]
struct Needs {
    transport: Option<&'static str>,
    session: Option<&'static str>,
}

impl Needs {
    const fn of(&mut self, source: Need) -> &mut Option<&'static str> {
        match source {
            Need::Transport => &mut self.transport,
            Need::Session => &mut self.session,
        }
    }

    const fn any(&self) -> bool {
        self.transport.is_some() || self.session.is_some()
    }
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
    /// on `runtime`, and they rest again `rest_after` after nothing needs
    /// them ([`REST_AFTER`] in a session). An empty `relays` holds nothing
    /// back.
    #[must_use]
    pub fn defer(
        endpoint: &Endpoint,
        relays: &RelayMap,
        runtime: tokio::runtime::Handle,
        rest_after: Duration,
    ) -> Self {
        let relays: Vec<Arc<RelayConfig>> = relays.relays();
        if relays.is_empty() {
            return Self::default();
        }
        Self {
            held: Arc::new_cyclic(|cell| {
                Mutex::new(Some(Held {
                    cell: cell.clone(),
                    endpoint: endpoint.clone(),
                    relays,
                    runtime,
                    rest_after,
                    awake: false,
                    needs: Needs::default(),
                    needed_at: Instant::now(),
                    rest_pending: false,
                    applied: Arc::default(),
                }))
            }),
        }
    }

    /// Whether relays are held back now.
    #[must_use]
    pub fn is_dormant(&self) -> bool {
        with_held(&self.held, |held| !held.awake).unwrap_or(false)
    }

    /// Whether this handle holds relays back at all, so what needs them is
    /// worth reporting.
    #[must_use]
    pub fn holds_relays(&self) -> bool {
        with_held(&self.held, |_| ()).is_some()
    }

    /// What holds the relays up now, by source, for a test to say why they
    /// did not rest.
    #[cfg(test)]
    pub(crate) fn holders(&self) -> (Option<&'static str>, Option<&'static str>) {
        with_held(&self.held, |held| {
            (held.needs.transport, held.needs.session)
        })
        .unwrap_or_default()
    }

    /// Start the held-back relays, because of `why`, or keep them up a
    /// further [`REST_AFTER`] if they run. Returns whether this call started
    /// them; a call on a handle that holds nothing back does nothing. Safe
    /// from any thread, and cheap enough to call on every dial.
    pub fn wake(&self, why: &'static str) -> bool {
        with_held(&self.held, |held| {
            let started = held.wake(why);
            held.rest_when_unneeded();
            started
        })
        .unwrap_or(false)
    }

    /// Record what `source` needs now: `Some(why)` holds the relays up (and
    /// starts them), `None` lets them rest [`REST_AFTER`] from now if
    /// nothing else needs them.
    pub fn need(&self, source: Need, why: Option<&'static str>) {
        with_held(&self.held, |held| held.need(source, why));
    }

    /// This device's address once it names a home relay, waiting up to
    /// `within` for one: an invite that carries it lets the invitee reach
    /// this device from behind any NAT at once, instead of after address
    /// lookup finds the relay. `None` when this handle holds nothing back
    /// (the endpoint's own address is as good as it gets) or no home relay
    /// arrived in time. Blocks the calling thread, which must not be one of
    /// the network runtime's.
    #[must_use]
    pub fn relay_addr_within(&self, within: Duration) -> Option<EndpointAddr> {
        let (endpoint, runtime) = with_held(&self.held, |held| {
            (held.endpoint.clone(), held.runtime.clone())
        })?;
        let addr = endpoint.addr();
        if addr.relay_urls().next().is_some() {
            return Some(addr);
        }
        let (found, answer) = std::sync::mpsc::sync_channel(1);
        runtime.spawn(async move {
            use iroh::Watcher as _;
            let mut watch = endpoint.watch_addr();
            let relayed = async {
                loop {
                    let addr = watch.get();
                    if addr.relay_urls().next().is_some() {
                        return Some(addr);
                    }
                    if watch.updated().await.is_err() {
                        return None;
                    }
                }
            };
            let _ = found.send(tokio::time::timeout(within, relayed).await.ok().flatten());
        });
        // The task answers by `within`; the margin covers its scheduling.
        answer
            .recv_timeout(within.saturating_add(Duration::from_millis(250)))
            .ok()
            .flatten()
    }
}

/// Run `f` on the state in `cell`, if it holds relays back, under its lock.
fn with_held<T>(cell: &Cell, f: impl FnOnce(&mut Held) -> T) -> Option<T> {
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_mut()
        .map(f)
}

impl Held {
    /// Mark the relays needed now, starting them if they rest.
    fn wake(&mut self, why: &'static str) -> bool {
        self.needed_at = Instant::now();
        if self.awake {
            return false;
        }
        self.awake = true;
        tracing::info!(
            why,
            relays = self.relays.len(),
            "peer-sync: this device needs to be reachable; starting its relays"
        );
        self.settle();
        true
    }

    fn need(&mut self, source: Need, why: Option<&'static str>) {
        let slot = self.needs.of(source);
        if *slot == why {
            return;
        }
        let ended = slot.is_some() && why.is_none();
        *slot = why;
        if let Some(why) = why {
            self.wake(why);
        } else if ended {
            self.needed_at = Instant::now();
        }
        self.rest_when_unneeded();
    }

    /// While the relays run and nothing holds them up, have one task rest
    /// them once [`Self::rest_after`] has passed since they were last
    /// needed. A later wake moves that moment on; a need cancels it.
    fn rest_when_unneeded(&mut self) {
        if !self.awake || self.rest_pending || self.needs.any() {
            return;
        }
        self.rest_pending = true;
        self.runtime.spawn(rest_when_due(self.cell.clone()));
    }

    /// Rest the relays if they have gone unneeded for their whole delay.
    /// `Some` is when to look again; `None` once there is nothing to wait
    /// for: they rested, or something needs them again.
    fn rest_if_due(&mut self) -> Option<Instant> {
        if !self.awake || self.needs.any() {
            self.rest_pending = false;
            return None;
        }
        let due = self
            .needed_at
            .checked_add(self.rest_after)
            .unwrap_or(self.needed_at);
        if Instant::now() < due {
            return Some(due);
        }
        self.awake = false;
        self.rest_pending = false;
        tracing::info!(
            relays = self.relays.len(),
            "peer-sync: nothing needs this device reachable any more; resting its relays"
        );
        self.settle();
        None
    }

    /// Bring the endpoint's relay map to what [`Self::awake`] says.
    fn settle(&self) {
        self.runtime
            .spawn(settle(self.cell.clone(), Arc::clone(&self.applied)));
    }
}

/// Rest the relays when they have gone unneeded for their whole delay, or
/// stop waiting once something needs them again.
async fn rest_when_due(cell: Weak<Mutex<Option<Held>>>) {
    loop {
        let Some(due) = cell
            .upgrade()
            .and_then(|cell| with_held(&cell, Held::rest_if_due))
            .flatten()
        else {
            return;
        };
        tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await;
    }
}

/// Insert or remove the relays until the endpoint's map matches what the
/// handle last asked for.
async fn settle(cell: Weak<Mutex<Option<Held>>>, applied: Arc<tokio::sync::Mutex<bool>>) {
    let mut applied = applied.lock().await;
    let Some((endpoint, relays, awake)) = cell.upgrade().and_then(|cell| {
        with_held(&cell, |held| {
            (held.endpoint.clone(), held.relays.clone(), held.awake)
        })
    }) else {
        return;
    };
    if *applied == awake {
        return;
    }
    for relay in relays {
        let url = relay.url.clone();
        if awake {
            endpoint.insert_relay(url, relay).await;
        } else {
            endpoint.remove_relay(&url).await;
        }
    }
    *applied = awake;
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

    use super::{Need, REST_AFTER, Reachability};

    /// Two relays nobody listens on: the test observes the relay map, not
    /// a relay connection.
    fn unused_relays() -> RelayMap {
        ["http://127.0.0.1:9/", "http://127.0.0.1:10/"]
            .into_iter()
            .map(|url| url.parse::<RelayUrl>().unwrap())
            .collect()
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
        let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while relays.len() != count {
            assert!(
                Instant::now() < deadline,
                "the endpoint's relay map never reached {count} relays"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn deferred(endpoint: &iroh::Endpoint, rest_after: Duration) -> Reachability {
        Reachability::defer(
            endpoint,
            &unused_relays(),
            tokio::runtime::Handle::current(),
            rest_after,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relays_wait_until_woken_and_wake_once() {
        let (endpoint, relays) = endpoint_without_relays().await;
        let reachability = deferred(&endpoint, REST_AFTER);
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

        assert!(
            !reachability.wake("again"),
            "relays that run are not started twice"
        );
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nothing_to_defer_is_already_reachable() {
        let (endpoint, _) = endpoint_without_relays().await;
        let reachability = Reachability::defer(
            &endpoint,
            &RelayMap::empty(),
            tokio::runtime::Handle::current(),
            REST_AFTER,
        );
        assert!(!reachability.is_dormant());
        assert!(!reachability.wake("a test"));
        reachability.need(Need::Session, Some("a test"));
        assert!(!reachability.is_dormant());
        assert!(!Reachability::default().is_dormant());
        assert!(
            Reachability::default()
                .relay_addr_within(Duration::from_secs(1))
                .is_none(),
            "a handle holding nothing back has no relay to wait for"
        );
        endpoint.close().await;
    }

    /// A wake with nothing holding the relays up lasts its delay, and then
    /// the endpoint's map is empty again: net report and QAD stop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relays_woken_for_a_moment_rest_after_the_delay() {
        let (endpoint, relays) = endpoint_without_relays().await;
        let rest_after = Duration::from_millis(300);
        let reachability = deferred(&endpoint, rest_after);
        let woken = Instant::now();
        assert!(reachability.wake("the invite composer opened"));
        until_relays(&relays, 2).await;
        until_relays(&relays, 0).await;
        assert!(
            woken.elapsed() >= rest_after,
            "the relays rested before their delay"
        );
        assert!(reachability.is_dormant());
        assert!(
            reachability.wake("a second invite"),
            "rested relays start again"
        );
        until_relays(&relays, 2).await;
        endpoint.close().await;
    }

    /// A need holds the relays up however long it lasts; once it ends they
    /// rest only after the delay, and a need from any source keeps them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relays_stay_up_while_needed_and_rest_the_delay_after() {
        let (endpoint, relays) = endpoint_without_relays().await;
        let rest_after = Duration::from_millis(300);
        let reachability = deferred(&endpoint, rest_after);
        reachability.need(Need::Transport, Some("a peer on a topic"));
        reachability.need(Need::Session, Some("a room shared with a peer"));
        assert!(!reachability.is_dormant(), "a need starts the relays");
        until_relays(&relays, 2).await;

        tokio::time::sleep(rest_after.saturating_mul(3)).await;
        assert!(!reachability.is_dormant());
        reachability.need(Need::Transport, None);
        tokio::time::sleep(rest_after.saturating_mul(3)).await;
        assert!(
            !reachability.is_dormant(),
            "the session still needs them, so the peer leaving does not rest them"
        );
        assert_eq!(relays.len(), 2);

        let unneeded = Instant::now();
        reachability.need(Need::Session, None);
        until_relays(&relays, 0).await;
        assert!(
            unneeded.elapsed() >= rest_after,
            "the relays rested before their delay"
        );
        assert!(reachability.is_dormant());
        endpoint.close().await;
    }

    /// Every wake moves the moment they rest: relays in use do not stop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn each_wake_holds_the_relays_a_further_delay() {
        let (endpoint, relays) = endpoint_without_relays().await;
        let rest_after = Duration::from_secs(2);
        let reachability = deferred(&endpoint, rest_after);
        assert!(reachability.wake("dialling a peer"));
        until_relays(&relays, 2).await;
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(!reachability.wake("dialling a peer"));
        }
        assert!(
            !reachability.is_dormant(),
            "relays woken again before their delay passed rested"
        );
        until_relays(&relays, 0).await;
        endpoint.close().await;
    }

    /// Woken by an invite, an endpoint names the relay it chose as home in
    /// its address, which the invite carries; resting empties the map and
    /// gives the home relay up, so the address stops naming it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_woken_endpoint_names_its_home_relay_within_the_wait() {
        let (_server, own) = super::test_support::local_relay().await;
        let relays = RelayMap::empty();
        let endpoint = iroh::Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Custom(relays.clone()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let reachability = Reachability::defer(
            &endpoint,
            &own,
            tokio::runtime::Handle::current(),
            Duration::from_millis(300),
        );
        assert_eq!(endpoint.addr().relay_urls().count(), 0);
        let waiting = reachability.clone();
        let named = tokio::task::spawn_blocking(move || {
            waiting.relay_addr_within(Duration::ZERO).is_none()
        })
        .await
        .unwrap();
        assert!(named, "a dormant endpoint has no home relay to name");

        reachability.wake("an invitation was issued");
        let waiting = reachability.clone();
        let addr = tokio::task::spawn_blocking(move || {
            waiting.relay_addr_within(tilo_test_env::hang_bound())
        })
        .await
        .unwrap()
        .expect("a woken endpoint picks a home relay");
        assert_eq!(
            addr.relay_urls().collect::<Vec<_>>(),
            own.urls::<Vec<_>>().iter().collect::<Vec<_>>()
        );

        until_relays(&relays, 0).await;
        assert!(reachability.is_dormant());
        let deadline = Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .unwrap();
        while endpoint.addr().relay_urls().count() != 0 {
            assert!(
                Instant::now() < deadline,
                "a rested endpoint still names the home relay it gave up"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        endpoint.close().await;
    }
}

/// A relay of a test's own, for tests that need a home relay to appear.
#[cfg(test)]
pub(crate) mod test_support {
    #![allow(
        clippy::expect_used,
        reason = "a test fixture that cannot start fails the test that needs it"
    )]

    use iroh_relay::server::{RelayConfig, Server, ServerConfig};

    /// A relay on loopback and a map naming it. It offers no QUIC address
    /// discovery, so a net report needs only its HTTP probe: an endpoint
    /// picks it as its home relay in a fraction of a second, where a probe
    /// waiting on QAD that never answers takes about three.
    pub(crate) async fn local_relay() -> (Server, iroh::RelayMap) {
        let mut config = ServerConfig::default();
        config.relay = Some(RelayConfig::new((std::net::Ipv4Addr::LOCALHOST, 0)));
        let server = Server::spawn(config)
            .await
            .expect("a loopback relay starts");
        let url: iroh::RelayUrl = format!(
            "http://{}",
            server.http_addr().expect("the relay serves HTTP")
        )
        .parse()
        .expect("a loopback relay URL");
        let mut relay = iroh::RelayConfig::from(url);
        relay.quic = None;
        (server, std::iter::once(relay).collect())
    }
}
