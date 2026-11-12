//! Where this device answers, as whoever it hands its address to dials it.
//!
//! An endpoint names a relay in its address only once it has connected to
//! one, and until it does, the address it signs names only what its own
//! network knows: a stranger on another network dials nothing. Both places a
//! device says where it is — a host naming its own address in a workspace's
//! governance, and a session naming its own endpoint in a link it hands out —
//! say it through these: wait for the relay before naming the address, and
//! follow the relays afterwards, so what was named stays dialable.

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
        loop {
            let addr = watch.get();
            if addr.relay_urls().next().is_some() {
                return addr;
            }
            if watch.updated().await.is_err() {
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

/// [`relay_addr`] for as long as a first net report and a relay handshake take.
pub async fn await_relay(endpoint: &Endpoint) -> EndpointAddr {
    relay_addr(endpoint, RELAY_WAIT).await
}

/// The relays `addr` says a device is reached through.
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
            // The watcher disconnects once the endpoint is gone.
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

    /// The relay that arrives while a host waits is the one it names.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiting_names_the_relay_the_endpoint_reached() {
        let (_server, own) = crate::test_support::local_relay().await;
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
        let (_server, own) = crate::test_support::local_relay().await;
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
