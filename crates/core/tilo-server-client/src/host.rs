//! Where a workspace's server is reached, and the one place that decides how a
//! device dials it.
//!
//! Every device reaches its server over iroh, whatever it runs on. A native
//! device dials the server's own addresses or the relay it names; a page has no
//! UDP socket, so every packet it sends goes through that relay. The relay is
//! part of the server's address ([`ServerAddress::addr`]), never a separate field beside
//! it, so whatever dials a server (the session, its live sessions, certification)
//! dials it the same way.
//!
//! How a device reaches it is its [`Carrier`]: a page's is the browser's
//! WebTransport, to the server's own listener ([`WebLink`]); a native device's
//! is iroh, direct unless it is asked to go through the relay. A native
//! device can take the page's way too, which is how a feature is shown to
//! work over every carrier ([`crate::link`] dials each).

use iroh_base::{EndpointAddr, RelayUrl};
use tilo_server_protocol::web::WebLink;

#[cfg(feature = "dial")]
use crate::LinkError;

/// How a device's packets reach its server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// The server's own addresses, and its relay when no direct path opens: a
    /// native device's.
    Direct,
    /// Only the packet relay the server names, over iroh.
    Relay,
    /// The server's WebTransport listener: a page's, with the browser's own
    /// transport, and a native device's when asked to go the page's way.
    WebTransport,
}

impl Carrier {
    /// The carrier this build has: a page reaches its server over the
    /// browser's WebTransport, a native device over iroh.
    pub const OWN: Self = if cfg!(target_family = "wasm") {
        Self::WebTransport
    } else {
        Self::Direct
    };
}

/// A workspace's server (`tilo-server`), as a device reaches it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerAddress {
    /// Its iroh endpoint, with the relay it is dialled through when it names
    /// one.
    addr: EndpointAddr,
    /// How this device reaches it.
    carrier: Carrier,
    /// Its WebTransport listener, when it serves browsers.
    web: Option<WebLink>,
}

impl ServerAddress {
    /// The server at `addr`, dialled through the relay `addr` names, if any.
    #[must_use]
    pub const fn at(addr: EndpointAddr) -> Self {
        Self {
            addr,
            carrier: Carrier::OWN,
            web: None,
        }
    }

    /// This server, reached over `carrier`.
    #[must_use]
    pub fn over(self, carrier: Carrier) -> Self {
        Self { carrier, ..self }
    }

    /// This server, taking WebTransport sessions at `web`. A page reaches a server
    /// that takes none (one behind a NAT, which no browser dials directly)
    /// through the packet relay its address names, over iroh.
    #[must_use]
    pub fn with_web(self, web: Option<WebLink>) -> Self {
        let carrier = if cfg!(target_family = "wasm")
            && self.carrier == Carrier::WebTransport
            && web.is_none()
            && self.relay().is_some()
        {
            Carrier::Relay
        } else {
            self.carrier
        };
        Self {
            carrier,
            web,
            ..self
        }
    }

    /// Where the server takes WebTransport sessions, when it does.
    #[must_use]
    pub const fn web(&self) -> Option<&WebLink> {
        self.web.as_ref()
    }

    /// How this device reaches the server.
    #[must_use]
    pub const fn carrier(&self) -> Carrier {
        self.carrier
    }

    /// The server at `addr`, dialled through `relay` when it names one, and
    /// otherwise through the relay `addr` names.
    #[must_use]
    pub fn through(addr: EndpointAddr, relay: Option<RelayUrl>) -> Self {
        Self::at(match relay {
            Some(relay) => addr.with_relay_url(relay),
            None => addr,
        })
    }

    /// The server where it says it is reached (`/server`).
    #[must_use]
    pub fn published(address: &tilo_server_protocol::web::ServerAddress) -> Self {
        let relay = address
            .relay_url
            .as_deref()
            .and_then(|url| url.parse::<RelayUrl>().ok());
        Self::through(address.endpoint_addr.clone(), relay).with_web(address.web.clone())
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

    /// The relay the server is dialled through, if it names one.
    #[must_use]
    pub fn relay(&self) -> Option<&RelayUrl> {
        self.addr.relay_urls().next()
    }

    /// An endpoint to dial this server from over its [`Carrier`], speaking as
    /// `key` (a fresh key when `None`), bound to the server's relay when it
    /// names one. Everything that dials a server binds here.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the endpoint does not bind, or, over
    /// the relay, when the server names none: an endpoint with no direct
    /// transport and no relay could never reach the server.
    #[cfg(feature = "dial")]
    pub async fn bind(&self, key: Option<[u8; 32]>) -> Result<iroh::Endpoint, LinkError> {
        self.bind_at(key, None).await
    }

    /// [`bind`](Self::bind), its direct transport bound on UDP `port` of every interface when one
    /// is given ([`listening_on`]), so a forward to that port reaches it; over the relay it has
    /// no direct transport, and `port` is not used.
    ///
    /// # Errors
    ///
    /// As [`bind`](Self::bind), and when `port` cannot be bound.
    #[cfg(feature = "dial")]
    pub async fn bind_at(
        &self,
        key: Option<[u8; 32]>,
        port: Option<u16>,
    ) -> Result<iroh::Endpoint, LinkError> {
        let relay = self.relay().cloned();
        // A page has no endpoint to reach its server from: its joins are its device service's.
        // A native device binds one whatever its links' carrier, since it takes a workspace in
        // itself, over the join protocol the host answers on its endpoint.
        #[cfg(target_arch = "wasm32")]
        if self.carrier == Carrier::WebTransport {
            return Err(LinkError::Unavailable(
                "a server reached over WebTransport is dialled without an endpoint".into(),
            ));
        }
        if self.carrier == Carrier::Relay && relay.is_none() {
            return Err(LinkError::Unavailable(
                "the server named no relay a page can reach it through".into(),
            ));
        }
        let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            // This endpoint dials a server and is never dialled: the server
            // answers on the connection this device opened, so there is no
            // port to ask a router for. Asking is a probe of the network —
            // UPnP over SSDP, PCP, NAT-PMP — which the device's network, or
            // a sandbox it runs in, need never see.
            .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled);
        // A page has only the relay; a native device asked to go through it
        // drops its own addresses.
        #[cfg(not(target_family = "wasm"))]
        if self.carrier == Carrier::Relay {
            // The relay's TCP connection paces what the network carries;
            // QUIC's own slow start above it only adds round trips.
            builder = builder.transport_config(transport()).clear_ip_transports();
        }
        #[cfg(not(target_family = "wasm"))]
        if let Some(port) = port.filter(|_| self.carrier != Carrier::Relay) {
            builder = listening_on(builder, port)
                .map_err(|error| LinkError::Unavailable(format!("UDP port {port}: {error}")))?;
        }
        #[cfg(target_family = "wasm")]
        let _ = port;
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
/// members, which its server writes to the connection at once. From QUIC's
/// default window (14,720 bytes, RFC 9002) slow start doubles it every round
/// trip, so that half megabyte took six round trips, every one a wait the
/// visitor sees; from 768 KiB it takes one. The same default paced the
/// device's own first flight, its hello, to a packet every 65 ms at a 400 ms
/// round trip, behind the handshake's own frames. A page reaches its server
/// through the server's packet relay, whose TCP connection paces what the
/// network carries and whose queue holds this much; a server's endpoint, which
/// devices also dial directly, is paced over the round trip, so the window
/// goes out as a steady stream rather than one burst.
#[cfg(all(feature = "dial", not(target_arch = "wasm32")))]
pub const INITIAL_WINDOW: u64 = 768 * 1024;

/// The QUIC transport of an endpoint that sends a workspace's replay or
/// reaches its server through the relay: iroh's, starting from
/// [`INITIAL_WINDOW`].
#[cfg(all(feature = "dial", not(target_arch = "wasm32")))]
#[must_use]
pub fn transport() -> iroh::endpoint::QuicTransportConfig {
    let mut cubic = noq_proto::congestion::CubicConfig::default();
    cubic.initial_window(INITIAL_WINDOW);
    iroh::endpoint::QuicTransportConfig::builder()
        .congestion_controller_factory(std::sync::Arc::new(cubic))
        .build()
}

/// `builder` with its direct transport on UDP `port` of every interface: IPv4, and IPv6 where the
/// machine has it. A device whose port is fixed is reached through a forward to it (a local VM's
/// host, a container's published port); the relay and the addresses it tells others stay as they
/// are.
///
/// # Errors
///
/// The address is not one iroh binds.
#[cfg(all(feature = "dial", not(target_family = "wasm")))]
pub fn listening_on(
    builder: iroh::endpoint::Builder,
    port: u16,
) -> Result<iroh::endpoint::Builder, iroh::endpoint::InvalidSocketAddr> {
    builder
        .clear_ip_transports()
        .bind_addr_with_opts(
            std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port)),
            iroh::endpoint::BindOpts::default(),
        )?
        .bind_addr_with_opts(
            std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)),
            iroh::endpoint::BindOpts::default().set_is_required(false),
        )
}

#[cfg(test)]
mod tests {

    use super::{Carrier, ServerAddress};
    use iroh_base::{EndpointAddr, RelayUrl, SecretKey};
    use tilo_server_protocol::web::ServerAddress as WebServerAddress;

    #[test]
    fn a_host_is_dialled_through_the_relay_its_address_names() {
        let key = SecretKey::from_bytes(&[7; 32]).public();
        let relay: RelayUrl = "https://relay.example".parse().unwrap();
        let named = ServerAddress::at(EndpointAddr::new(key).with_relay_url(relay.clone()));
        assert_eq!(named.relay(), Some(&relay));
        assert_eq!(ServerAddress::at(EndpointAddr::new(key)).relay(), None);
    }

    #[test]
    fn a_published_host_is_dialled_through_the_relay_it_publishes() {
        let key = SecretKey::from_bytes(&[7; 32]).public();
        let published = ServerAddress::published(&WebServerAddress {
            endpoint_addr: EndpointAddr::new(key),
            relay_url: Some("https://relay.example".into()),
            web: None,
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
        let direct = ServerAddress::at(addr.clone()).over(Carrier::Direct);
        assert_eq!(direct.addr(), addr);
        let relayed = ServerAddress::at(addr).over(Carrier::Relay);
        assert_eq!(relayed.addr(), EndpointAddr::new(key).with_relay_url(relay));
    }
}
