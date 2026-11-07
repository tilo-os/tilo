//! Finding the person's devices on the local network, without a relay.
//!
//! A Tilo endpoint dials a peer by its key. Number 0's directory answers
//! that over the internet, and only while this endpoint has a home relay to
//! be found at; a device whose relays rest, or a network without the
//! internet, leaves two of the person's devices holding each other's keys
//! and no way to turn one into an address. So the session endpoint also
//! advertises its direct addresses on the local network under Tilo's own
//! service name and answers a lookup of any key it hears advertised there.
//!
//! The service name is Tilo's, not iroh's, so only another Tilo endpoint
//! answers; direct addresses only, since a relay advertised here would need
//! the internet this lookup exists without. What crosses the network is the
//! key and the addresses — the same pair a directory record holds — and the
//! connection it leads to is authenticated as every iroh connection is.

use iroh::address_lookup::{AddrFilter, AddressLookupBuilder};
use iroh_mdns_address_lookup::MdnsAddressLookup;

/// The multicast service Tilo endpoints advertise under.
pub const SERVICE_NAME: &str = "tilo-v1";

/// The local-network lookup a session's endpoint takes beside number 0's:
/// it advertises this endpoint's direct addresses and resolves another
/// device's key to the address it advertises.
#[must_use]
pub fn lan() -> impl AddressLookupBuilder {
    MdnsAddressLookup::builder()
        .service_name(SERVICE_NAME)
        .addr_filter(AddrFilter::ip_only())
}

#[cfg(test)]
mod tests {
    use iroh::endpoint::presets;
    use iroh::{Endpoint, RelayMode};
    use tokio::time::{Duration, timeout};

    use super::lan;

    const ALPN: &[u8] = b"tilo/lan-discovery-test";

    /// Two endpoints on this network find each other by key alone, with no
    /// relay and no directory: only what the local lookup hears.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_endpoints_find_each_other_on_the_local_network() {
        let binding = || {
            Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .alpns(vec![ALPN.to_vec()])
                .address_lookup(lan())
        };
        let host = binding().bind().await.expect("the host endpoint binds");
        let guest = binding().bind().await.expect("the guest endpoint binds");

        let accepting = tokio::spawn({
            let host = host.clone();
            async move {
                let incoming = host.accept().await.expect("the host hears a connection");
                let connection = incoming.await.expect("the connection authenticates");
                let (_, mut receive) = connection.accept_bi().await.expect("a stream");
                let said = receive.read_to_end(1024).await.expect("the stream reads");
                assert_eq!(said, b"hello");
            }
        });
        let connection = timeout(Duration::from_secs(30), guest.connect(host.id(), ALPN))
            .await
            .expect("found on the local network")
            .expect("dialed");
        let (mut send, _) = connection.open_bi().await.expect("a stream");
        send.write_all(b"hello").await.expect("sent");
        send.finish().expect("finished");
        timeout(Duration::from_secs(30), accepting)
            .await
            .expect("heard")
            .expect("the host's task");
        guest.close().await;
        host.close().await;
    }
}
