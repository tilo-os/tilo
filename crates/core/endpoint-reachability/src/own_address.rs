//! Where this device answers, as whoever it hands its address to dials it.

use std::collections::BTreeSet;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, RelayUrl, Watcher as _};

/// How long naming a relay takes at most: about what iroh's first net report
/// and a relay handshake need before an endpoint has one.
pub const RELAY_WAIT: Duration = Duration::from_secs(iroh::NET_REPORT_TIMEOUT);

/// Where `endpoint` answers once it is reached through a relay: the address a
/// stranger dials. Waits up to `within` for one and answers the address it has
/// either way — bounded, so a device with no way out still names the address
/// it has.
pub async fn relay_addr(endpoint: &Endpoint, within: Duration) -> EndpointAddr {
    let relayed = async {
        let mut watch = endpoint.watch_addr();
        let mut relays = endpoint.home_relay_status();
        loop {
            let addr = watch.get();
            if addr.relay_urls().next().is_some() {
                return addr;
            }
            // No relay of this endpoint's answers: every one it was given has failed, and the
            // address it has is the one to name. Waiting the whole time would hold a ticket
            // (or a server) back for a relay that is not there.
            let statuses = relays.get();
            if !statuses.is_empty()
                && statuses
                    .iter()
                    .all(|relay| !relay.is_connected() && relay.last_error().is_some())
            {
                return addr;
            }
            let changed = n0_future::future::or(async { watch.updated().await.is_ok() }, async {
                relays.updated().await.is_ok()
            })
            .await;
            if !changed {
                return addr;
            }
        }
    };
    let Ok(addr) = tokio::time::timeout(within, relayed).await else {
        tracing::warn!("the endpoint reached no relay; its address names its own network alone");
        return endpoint.addr();
    };
    addr
}

pub async fn await_relay(endpoint: &Endpoint) -> EndpointAddr {
    relay_addr(endpoint, RELAY_WAIT).await
}

/// Whether an endpoint can name where it is reached, as whoever bound it knows.
///
/// A server waits for that name before it serves, so no workspace it opens
/// learns an address it would have to rename. An endpoint reached on its own
/// network only never names one, and waiting for it holds the server back for
/// the whole wait before it serves a workspace that would have named the
/// address it has all along.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reaching {
    /// The endpoint is expected to name where it is reached through another
    /// machine, which may not have answered yet.
    #[default]
    ByRelay,
    /// Nothing will name it: its network is its own, and nobody reaches it
    /// from outside.
    OnItsOwn,
    /// It names where it is reached once something needs it reachable, and
    /// nothing may wait for that: a desktop, whose first frame waits on what
    /// it serves and whose endpoint dials out only once it must. What it
    /// serves names the address again once it has one ([`follow`]).
    WhenNeeded,
}

impl Reaching {
    /// Wait for `endpoint` to name where it is reached when it is expected to,
    /// and answer at once when nothing will, or nothing may wait for it.
    pub async fn awaited(self, endpoint: &Endpoint) {
        if self == Self::ByRelay {
            await_relay(endpoint).await;
        }
    }
}

fn relays_of(addr: &EndpointAddr) -> BTreeSet<RelayUrl> {
    addr.relay_urls().cloned().collect()
}

/// Call `each` with `endpoint`'s address whenever the relays it is reached
/// through change: what a host that named its own address watches, so what it
/// named moves with it. A change to an empty set is not one: a host that lost
/// its relays keeps the address it named, and is reached again through the
/// relay once it comes back. Watching runs on `runtime`; abort the answer to
/// stop watching.
pub fn follow<F, Fut>(
    endpoint: &Endpoint,
    runtime: &tokio::runtime::Handle,
    mut each: F,
) -> tokio::task::AbortHandle
where
    F: FnMut(EndpointAddr) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut watch = endpoint.watch_addr();
    let mut relays = relays_of(&watch.get());
    let following = runtime.spawn(async move {
        loop {
            if watch.updated().await.is_err() {
                return;
            }
            let addr = watch.get();
            let now = relays_of(&addr);
            if now == relays || now.is_empty() {
                continue;
            }
            relays = now;
            each(addr).await;
        }
    });
    following.abort_handle()
}

#[cfg(test)]
mod tests {
    use iroh::RelayMode;
    use tokio::runtime::Handle;

    use super::*;
    use crate::Reachability;

    /// An endpoint with no relay in its map: what a device that has nowhere
    /// to be reached from binds.
    async fn endpoint_without_relays() -> Endpoint {
        Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(RelayMode::Custom(iroh::RelayMap::empty()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap()
    }

    /// A device that reached no relay answers the address it has, and warns
    /// that it carries its own network alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_endpoint_with_no_relay_answers_the_address_it_has() {
        let endpoint = endpoint_without_relays().await;
        let addr = relay_addr(&endpoint, Duration::ZERO).await;
        assert_eq!(addr.id, endpoint.id());
        assert_eq!(addr.relay_urls().count(), 0);
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiting_names_the_relay_the_endpoint_reached() {
        let relay = tilo_test_relay::relay().await;
        let own = relay.relays();
        let endpoint = endpoint_without_relays().await;
        let reachability =
            Reachability::defer(&endpoint, &own, tokio::runtime::Handle::current(), None);
        assert!(reachability.wake("a test"));
        let addr = relay_addr(&endpoint, Duration::from_secs(30)).await;
        assert_eq!(
            addr.relay_urls().collect::<Vec<_>>(),
            own.urls::<Vec<_>>().iter().collect::<Vec<_>>()
        );
        endpoint.close().await;
    }

    /// A host that was named without a relay is told again once it has one,
    /// and not for the addresses beside it: net reports move those without
    /// changing who reaches it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn following_names_each_relay_change_and_nothing_else() {
        let relay = tilo_test_relay::relay().await;
        let own = relay.relays();
        let endpoint = endpoint_without_relays().await;
        let reachability = Reachability::defer(&endpoint, &own, Handle::current(), None);
        let (told, heard) = std::sync::mpsc::channel();
        let following = follow(&endpoint, &Handle::current(), move |addr| {
            let told = told.clone();
            async move {
                let _ = told.send(addr);
            }
        });
        assert!(reachability.wake("a test"));
        let named = heard
            .recv_timeout(Duration::from_secs(30))
            .expect("the change is named");
        assert!(
            named
                .relay_urls()
                .any(|url| own.urls::<Vec<_>>().contains(url)),
            "the address follows the relay it reached: {named:?}"
        );
        assert!(
            heard.recv_timeout(Duration::from_millis(250)).is_err(),
            "nothing else is named while the relays stay the same"
        );
        following.abort();
        endpoint.close().await;
    }
}
