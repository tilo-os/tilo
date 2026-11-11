//! Where a workspace's host is reached, and the one place that decides how a
//! device dials it.
//!
//! Every device reaches its host over iroh, whatever it runs on. A native
//! device dials the host's own addresses or the relay it names; a page has no
//! UDP socket, so every packet it sends goes through that relay. The relay is
//! part of the host's address ([`Host::addr`]), never a separate field beside
//! it, so whatever dials a host (the session, its planes, certification)
//! dials it the same way.
//!
//! How a device reaches it is its [`Carrier`]: a page's is always the relay; a
//! native device's is direct unless it is asked to go the page's way, which
//! is how a feature is shown to work over both.

use iroh_base::{EndpointAddr, RelayUrl};

#[cfg(feature = "iroh")]
use crate::LinkError;

/// How a device's packets reach its host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// The host's own addresses, and its relay when no direct path opens: a
    /// native device's.
    Direct,
    /// Only the relay the host names: a page's, which holds no UDP socket.
    Relay,
}

impl Carrier {
    /// The carrier this build has: a page reaches its host through a relay
    /// only.
    pub const OWN: Self = if cfg!(target_family = "wasm") {
        Self::Relay
    } else {
        Self::Direct
    };
}

/// A workspace's host (`tilo-relay`), as a device reaches it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    /// Its iroh endpoint, with the relay it is dialled through when it names
    /// one.
    addr: EndpointAddr,
    /// How this device reaches it.
    carrier: Carrier,
}

impl Host {
    /// The host at `addr`, dialled through the relay `addr` names, if any.
    #[must_use]
    pub const fn at(addr: EndpointAddr) -> Self {
        Self {
            addr,
            carrier: Carrier::OWN,
        }
    }

    /// This host, reached over `carrier`.
    #[must_use]
    pub fn over(self, carrier: Carrier) -> Self {
        Self { carrier, ..self }
    }

    /// How this device reaches the host.
    #[must_use]
    pub const fn carrier(&self) -> Carrier {
        self.carrier
    }

    /// The host at `addr`, dialled through `relay` when it names one, and
    /// otherwise through the relay `addr` names.
    #[must_use]
    pub fn through(addr: EndpointAddr, relay: Option<RelayUrl>) -> Self {
        Self::at(match relay {
            Some(relay) => addr.with_relay_url(relay),
            None => addr,
        })
    }

    /// The host where it says it is reached (`/host`).
    #[must_use]
    pub fn published(address: &tilo_relay_protocol::web::HostAddress) -> Self {
        let relay = address
            .relay_url
            .as_deref()
            .and_then(|url| url.parse::<RelayUrl>().ok());
        Self::through(address.endpoint_addr.clone(), relay)
    }

    /// The address a device dials: over the relay, the endpoint's key and its
    /// relay alone.
    #[must_use]
    pub fn addr(&self) -> EndpointAddr {
        match (self.carrier, self.relay()) {
            (Carrier::Relay, Some(relay)) => {
                EndpointAddr::new(self.addr.id).with_relay_url(relay.clone())
            }
            _ => self.addr.clone(),
        }
    }

    /// The relay the host is dialled through, if it names one.
    #[must_use]
    pub fn relay(&self) -> Option<&RelayUrl> {
        self.addr.relay_urls().next()
    }

    /// An endpoint to dial this host from over its [`Carrier`], speaking as
    /// `key` (a fresh key when `None`), bound to the host's relay when it
    /// names one. Everything that dials a host binds here.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the endpoint does not bind, or, over
    /// the relay, when the host names none: an endpoint with no direct
    /// transport and no relay could never reach the host.
    #[cfg(feature = "iroh")]
    pub async fn bind(&self, key: Option<[u8; 32]>) -> Result<iroh::Endpoint, LinkError> {
        let relay = self.relay().cloned();
        if self.carrier == Carrier::Relay && relay.is_none() {
            return Err(LinkError::Unavailable(
                "the host named no relay a page can reach it through".into(),
            ));
        }
        let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal);
        if self.carrier == Carrier::Relay {
            // The relay's TCP connection paces what the network carries;
            // QUIC's own slow start above it only adds round trips.
            builder = builder.transport_config(transport());
            #[cfg(not(target_family = "wasm"))]
            {
                builder = builder.clear_ip_transports();
            }
        }
        if let Some(key) = key {
            builder = builder.secret_key(iroh::SecretKey::from_bytes(&key));
        }
        if let Some(url) = relay {
            builder = builder.relay_mode(iroh::RelayMode::Custom(url.into()));
        }
        builder
            .bind()
            .await
            .map_err(|error| LinkError::Unavailable(error.to_string()))
    }
}

/// How much an endpoint sends on a new connection before its first
/// acknowledgement: the window QUIC's slow start begins from, and the window
/// its pacer spreads over a round trip.
///
/// A device entering a workspace waits first for its replay: the workspace's
/// governance and its shared desktop, about half a megabyte for a few hundred
/// members, which its host writes to the connection at once. From QUIC's
/// default window (14,720 bytes, RFC 9002) slow start doubles it every round
/// trip, so that half megabyte took six round trips, every one a wait the
/// visitor sees; from 256 KiB it takes two. The same default paced the
/// device's own first flight, its hello, to a packet every 65 ms at a 400 ms
/// round trip, behind the handshake's own frames. A page reaches its host
/// through the host's packet relay, whose TCP connection paces what the
/// network carries and whose queue holds this much; a host's endpoint, which
/// devices also dial directly, is paced over the round trip, so the window
/// goes out as a steady stream rather than one burst.
#[cfg(feature = "iroh")]
pub const INITIAL_WINDOW: u64 = 256 * 1024;

/// The QUIC transport of an endpoint that sends a workspace's replay or
/// reaches its host through the relay: iroh's, starting from
/// [`INITIAL_WINDOW`].
#[cfg(feature = "iroh")]
#[must_use]
pub fn transport() -> iroh::endpoint::QuicTransportConfig {
    let mut cubic = noq_proto::congestion::CubicConfig::default();
    cubic.initial_window(INITIAL_WINDOW);
    iroh::endpoint::QuicTransportConfig::builder()
        .congestion_controller_factory(std::sync::Arc::new(cubic))
        .build()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a test's parse is its assertion")]

    use super::{Carrier, Host};
    use iroh_base::{EndpointAddr, RelayUrl, SecretKey};
    use tilo_relay_protocol::web::HostAddress;

    #[test]
    fn a_host_is_dialled_through_the_relay_its_address_names() {
        let key = SecretKey::from_bytes(&[7; 32]).public();
        let relay: RelayUrl = "https://relay.example".parse().unwrap();
        let named = Host::at(EndpointAddr::new(key).with_relay_url(relay.clone()));
        assert_eq!(named.relay(), Some(&relay));
        assert_eq!(Host::at(EndpointAddr::new(key)).relay(), None);
    }

    #[test]
    fn a_published_host_is_dialled_through_the_relay_it_publishes() {
        let key = SecretKey::from_bytes(&[7; 32]).public();
        let published = Host::published(&HostAddress {
            endpoint_addr: EndpointAddr::new(key),
            relay_url: Some("https://relay.example".into()),
        });
        assert_eq!(
            published.relay(),
            Some(&"https://relay.example".parse::<RelayUrl>().unwrap())
        );
        assert_eq!(published.addr().id, key);
    }

    #[test]
    fn over_the_relay_a_host_is_dialled_by_its_key_and_relay_alone() {
        let key = SecretKey::from_bytes(&[7; 32]).public();
        let relay: RelayUrl = "https://relay.example".parse().unwrap();
        let addr = EndpointAddr::new(key)
            .with_relay_url(relay.clone())
            .with_ip_addr("127.0.0.1:4433".parse().unwrap());
        let direct = Host::at(addr.clone()).over(Carrier::Direct);
        assert_eq!(direct.addr(), addr);
        let relayed = Host::at(addr).over(Carrier::Relay);
        assert_eq!(relayed.addr(), EndpointAddr::new(key).with_relay_url(relay));
    }
}
