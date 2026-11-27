//! This computer's own login, as `tilo login` keeps it, and how a computer becomes one of the
//! person's devices with a ticket ([`Ticket`]).
//!
//! A login is what the login reaches, with this computer's one device key and the certificate
//! the person's root signed for it kept where every Tilo program of this computer keeps them
//! ([`IdentityPaths`]). Every program of the login that acts as the person reads it here: `tilo`,
//! and `tilolang repl`.

mod ticket;

pub use ticket::*;

use serde::{Deserialize, Serialize};

/// Where a host's iroh endpoint is dialled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAddress {
    /// The endpoint's key and the addresses it answers at.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// The packet relay it is reached through, when it names one; a page dials
    /// the endpoint through it rather than over the network's own relays.
    pub relay_url: Option<String>,
    /// Where the host takes WebTransport sessions: how a page reaches it,
    /// with the browser's own transport. `None` from a host that serves no
    /// browsers.
    #[serde(default)]
    pub web: Option<WebLink>,
}

/// A host's WebTransport listener: HTTP/3 on the UDP port of its HTTPS
/// origin, presenting the same certificate. A device opens one session per
/// thing it asks, at that thing's path (`LINK_RECORDS_PATH`,
/// `LINK_ACTIONS_PATH`, `LINK_LIVE_SESSION_PATH`): the
/// same conversations a native device holds over the host's iroh endpoint on
/// their ALPNs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebLink {
    /// `https://host:port`, the origin the sessions open at.
    pub origin: String,
    /// SHA-256 of the certificate the person's local CA on the host issued,
    /// which a browser that does not trust that CA pins
    /// (`serverCertificateHashes`); `None` for a certificate the system trust
    /// bundle verifies.
    pub certificate_sha256: Option<[u8; 32]>,
}

#[cfg(not(target_arch = "wasm32"))]
pub mod daemon;
#[cfg(not(target_arch = "wasm32"))]
mod kept;

#[cfg(not(target_arch = "wasm32"))]
pub use kept::*;
