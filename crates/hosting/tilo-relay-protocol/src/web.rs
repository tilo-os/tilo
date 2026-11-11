//! What a page asks a host over HTTPS before it has a session: the host's
//! address and the join steps of a workspace. Served by the host itself.
//!
//! Everything a page does once it is up goes over its iroh session; these
//! routes only get it there. Every body is postcard. Routes (all `POST`,
//! below [`API_PREFIX`]):
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | `/host` | — | [`HostInfo`] |
//! | `/invitation` ([`INVITATION_PATH`]) | a signed invitation (`Vec<u8>`) | [`JoinReply`](crate::workspaces::JoinReply) |
//! | `/workspace-deleted` ([`WORKSPACE_DELETED_PATH`]) | a workspace's id (`String`) | `bool` |
//! | `/fetch` ([`FETCH_PATH`]) | any method: `tilo_net::proxy` wire protocol v1 | the target's response, or the proxy's failure |
//!
//! `/fetch` is the host's fetch proxy: through it a page reads the sites that
//! send no CORS headers. Unlike every other route it takes the method of the
//! request it relays, and it takes its session token in [`SESSION_HEADER`].
//!
//! A host started without a certificate serves only `/install` until it has
//! one: the body is the root-signed `DeviceCert` for its device key as JSON
//! (what a page's root issues) or in its canonical encoding, or
//! [`InstallCertificates`], with the install token the host was
//! provisioned with in [`INSTALL_TOKEN_HEADER`]. `200` answers
//! [`Installed`] as JSON; every other route answers `503` meanwhile. A
//! `GET` with the token answers [`InstallInfo`]: the host's key and, on a
//! node, the key of the session beside it, which the root certifies at the
//! same time as one of the person's devices.
//!
//! `/join` is the route a device calls without a session: a person a host
//! does not know yet joins through it. A device certifies itself, and a
//! member's root answers their own device's request, over the host's
//! certification ALPN (`crate::certify`).

use serde::{Deserialize, Serialize};

/// Path prefix of every route a page asks a host.
pub const API_PREFIX: &str = "/peer/v1";
/// Header carrying the session token.
pub const SESSION_HEADER: &str = "x-tilo-session";
/// Header carrying the install token of a host awaiting its certificate.
pub const INSTALL_TOKEN_HEADER: &str = "x-tilo-install-token";
/// The route that installs a waiting host's certificate.
pub const INSTALL_ROUTE: &str = "/install";

/// What a waiting host says about itself on `GET /install` (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallInfo {
    pub device_public_key: String,
    /// On a node, the session that runs the person's apps and agents beside
    /// the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<NodeSession>,
}

/// The device key of the session beside a node, vouched for by the node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSession {
    pub device_public_key: String,
    /// The node's device key's Ed25519 signature over
    /// [`node_session_message`], hex. A page that got the node's key out of
    /// band certifies the session's key only when this verifies.
    pub signature: String,
}

/// What a node signs to vouch for the session beside it.
#[must_use]
pub fn node_session_message(node: &[u8; 32], session: &[u8; 32]) -> Vec<u8> {
    let mut message = b"tilo node session v1\0".to_vec();
    message.extend_from_slice(node);
    message.extend_from_slice(session);
    message
}

/// `POST /install` with the certificates of a node and of the session
/// beside it (JSON): each in its canonical encoding, base64, as a page's
/// `certifyDevice` resolves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallCertificates {
    pub certificate: String,
    /// The session's certificate, when the host has one beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_certificate: Option<String>,
}

/// A waiting host's answer to an accepted `/install` (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub device_public_key: String,
    /// The identity it now serves, hex.
    pub user_public_key: String,
}

/// Largest request body a route accepts.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// What a page needs to know about the host before it has a certificate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// The host's device certificate (pin its device key).
    pub host_cert: tilo_user_identity::DeviceCert,
    /// Where the host listens on iroh, which is how a page reaches it for
    /// everything a session and its planes carry. `None` in a host that serves
    /// no iroh endpoint.
    pub address: Option<HostAddress>,
}

/// Where a host's iroh endpoint is dialled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostAddress {
    /// The endpoint's key and the addresses it answers at.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// The packet relay it is reached through, when it names one; a page dials
    /// the endpoint through it rather than over the network's own relays.
    pub relay_url: Option<String>,
}

/// [`HostInfo`] as a page reads it (JSON): what `hostInfo()` in the page's
/// runtime resolves to, and what a site hands its page about the host it
/// opens (`entry.json`, from the web entry's `TILO_WEB_HOME_HOST_INFO`), so the
/// page dials that host without asking it first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHost {
    /// The host's device certificate.
    pub host_cert: tilo_user_identity::DeviceCert,
    /// Where the host listens on iroh, when it names an endpoint.
    pub address: Option<PageHostAddress>,
}

/// [`HostAddress`] as a page reads it (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHostAddress {
    /// The endpoint's key and the addresses it answers at.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// The packet relay the host is reached through, when it names one.
    #[serde(default)]
    pub relay_url: Option<String>,
}

impl From<HostAddress> for PageHostAddress {
    fn from(address: HostAddress) -> Self {
        Self {
            endpoint_addr: address.endpoint_addr,
            relay_url: address.relay_url,
        }
    }
}

impl From<PageHostAddress> for HostAddress {
    fn from(address: PageHostAddress) -> Self {
        Self {
            endpoint_addr: address.endpoint_addr,
            relay_url: address.relay_url,
        }
    }
}

impl From<HostInfo> for PageHost {
    fn from(info: HostInfo) -> Self {
        Self {
            host_cert: info.host_cert,
            address: info.address.map(Into::into),
        }
    }
}

impl HostInfo {
    /// What a site hands its page about this host: its certificate, its
    /// endpoint's key and the packet relay pages reach it through, without
    /// the network addresses it answers at. A page has no UDP socket to dial
    /// those, and they change whenever the host starts somewhere else, while
    /// the rest stays as long as the host keeps its identity.
    #[must_use]
    pub fn for_site(&self) -> PageHost {
        let address = self.address.as_ref().map(|address| {
            let mut endpoint_addr = iroh_base::EndpointAddr::new(address.endpoint_addr.id);
            if let Some(relay) = address
                .relay_url
                .as_deref()
                .and_then(|url| url.parse::<iroh_base::RelayUrl>().ok())
            {
                endpoint_addr = endpoint_addr.with_relay_url(relay);
            }
            PageHostAddress {
                endpoint_addr,
                relay_url: address.relay_url.clone(),
            }
        });
        PageHost {
            host_cert: self.host_cert.clone(),
            address,
        }
    }
}

/// Whether its owner deleted the workspace a device names by its id, below
/// [`API_PREFIX`]: what a device the host no longer lets in (it was a member
/// of only that workspace) learns about it. Any device that names it is told:
/// the id is random, so naming it says the device already knew it.
pub const WORKSPACE_DELETED_PATH: &str = "/workspace-deleted";

/// What an invitation offers, below [`API_PREFIX`]: answered to anyone who
/// holds it, before they have a device to enter with
/// ([`crate::workspaces`]).
pub const INVITATION_PATH: &str = "/invitation";

/// The host's fetch proxy, below [`API_PREFIX`].
pub const FETCH_PATH: &str = "/fetch";

/// Bound, in characters, of the display name a device asks a workspace's door
/// to let its person in under.
pub const MAX_DISPLAY_NAME_CHARS: usize = 32;

/// `text` without control characters, cut to `max` characters: what a device
/// says of itself, from text that is not trusted to be printable.
#[must_use]
pub fn kept(text: &str, max: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(max)
        .collect()
}
