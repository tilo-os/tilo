//! What a page asks a host over HTTPS before it has a session: the host's
//! address and the join steps of a workspace. Served by the host itself.
//!
//! Everything a page does once it is up goes over its iroh session; these
//! routes only get it there. Every body is postcard. Routes (all `POST`,
//! below [`API_PREFIX`]):
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | `/host` | — | [`ServerInfo`] |
//! | `/invitation` ([`INVITATION_PATH`]) | a signed invitation (`Vec<u8>`) | [`JoinReply`](crate::workspaces::JoinReply) |
//! | `/invitation/open` ([`OPEN_INVITATION_PATH`]) | [`KeptInvitation`] as JSON | [`OpenedInvitation`] as JSON, or `410` |
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
//! A person a host does not know yet comes in on their device's session
//! ([`crate::entering`]), through a workspace's entry or with an invitation,
//! not on a route here. A device that holds no certificate asks for one with a
//! ticket, over the link a page reaches the host's actions on
//! (`tilo_device_identity::login`).

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
pub struct ServerInfo {
    /// The host's device certificate (pin its device key).
    #[serde(rename = "host_cert")]
    pub server_cert: tilo_user_identity::DeviceCert,
    /// Where the host listens on iroh, which is how a page reaches it for
    /// everything a session and its live sessions carry. `None` in a host that serves
    /// no iroh endpoint.
    pub address: Option<ServerAddress>,
    /// The host's HTTPS origin (`https://eu1.tilo.sh`): its name, where its
    /// pages and the links it makes are. Its WebTransport listener may answer
    /// at another (`address.web`). `None` from a host that serves no web.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// Where a host's iroh endpoint is dialled, kept with a device's login.
pub use tilo_device_identity::login::{ServerAddress, WebLink};

/// [`ServerInfo`] as a page reads it (JSON): what `hostInfo()` in the page's
/// runtime resolves to, and what a site hands its page about the host it
/// opens (`entry.json`, from the web entry's `TILO_WEB_HOME_HOST_INFO`), so the
/// page dials that host without asking it first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHost {
    /// The host's device certificate.
    #[serde(rename = "hostCert")]
    pub server_cert: tilo_user_identity::DeviceCert,
    /// Where the host listens on iroh, when it names an endpoint.
    pub address: Option<PageHostAddress>,
    /// The host's HTTPS origin ([`ServerInfo::origin`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// [`ServerAddress`] as a page reads it (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHostAddress {
    /// The endpoint's key and the addresses it answers at.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// The packet relay the host is reached through, when it names one.
    #[serde(default)]
    pub relay_url: Option<String>,
    /// The host's WebTransport listener: how a page reaches it.
    #[serde(default)]
    pub web: Option<WebLink>,
}

impl From<ServerAddress> for PageHostAddress {
    fn from(address: ServerAddress) -> Self {
        Self {
            endpoint_addr: address.endpoint_addr,
            relay_url: address.relay_url,
            web: address.web,
        }
    }
}

impl From<PageHostAddress> for ServerAddress {
    fn from(address: PageHostAddress) -> Self {
        Self {
            endpoint_addr: address.endpoint_addr,
            relay_url: address.relay_url,
            web: address.web,
        }
    }
}

impl From<ServerInfo> for PageHost {
    fn from(info: ServerInfo) -> Self {
        Self {
            server_cert: info.server_cert,
            address: info.address.map(Into::into),
            origin: info.origin,
        }
    }
}

impl From<PageHost> for ServerInfo {
    fn from(host: PageHost) -> Self {
        Self {
            server_cert: host.server_cert,
            address: host.address.map(Into::into),
            origin: host.origin,
        }
    }
}

impl ServerInfo {
    /// What a site hands its page about this host: its certificate, its
    /// endpoint's key, the packet relay and the WebTransport listener a page
    /// reaches it at, without the network addresses its endpoint answers at.
    /// Those change whenever the host starts somewhere else, while the rest
    /// stays as long as the host keeps its identity (and, for a pinned
    /// certificate, until it renews it).
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
                web: address.web.clone(),
            }
        });
        PageHost {
            server_cert: self.server_cert.clone(),
            address,
            origin: self.origin.clone(),
        }
    }

    /// This host's `web-host.json`, as the host writes it in its state: what
    /// a site hands its page about it and what an API server is configured
    /// with (`TILO_API_HOST`).
    ///
    /// # Errors
    /// Only as `serde_json` may fail to write a plain struct.
    pub fn web_host_json(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(&self.for_site())
    }
}

/// A workspace's live session: the first stream proves which device opened
/// the session (the relay handshake, `crate::auth`), then the live session's own
/// admission and `moq-lite` follow, as over iroh.
pub const LINK_LIVE_SESSION_PATH: &str = "/peer/v1/link/live-session";
/// A workspace's apps as web pages ([`crate::page`]): the first stream proves
/// which device opened the session (the handshake of `crate::auth`), then
/// carries the pages it asks for and what its person does on them.
pub const LINK_PAGE_PATH: &str = "/peer/v1/link/page";
/// A device's link to another device of its workspace over WebTransport: the
/// first stream proves which device opened the session (the handshake of
/// `crate::auth`), then the link's own streams carry what the same link
/// carries on [`crate::transport::DIRECT_ALPN`].
pub const LINK_RECORDS_PATH: &str = "/peer/v1/link/records";
/// One device asking another of its workspace to run an action, over
/// WebTransport: the first stream proves the asking device, then the actions
/// protocol's own streams follow, as on [`crate::transport::ACTIONS_ALPN`].
pub const LINK_ACTIONS_PATH: &str = "/peer/v1/link/actions";

/// Whether its owner deleted the workspace a device names by its id, below
/// [`API_PREFIX`]: what a device the host no longer lets in (it was a member
/// of only that workspace) learns about it. Any device that names it is told:
/// the id is random, so naming it says the device already knew it.
pub const WORKSPACE_DELETED_PATH: &str = "/workspace-deleted";

/// What an invitation offers, below [`API_PREFIX`]: answered to anyone who
/// holds it, before they have a device to enter with
/// ([`crate::workspaces`]).
pub const INVITATION_PATH: &str = "/invitation";

/// The invitation a workspace keeps, below [`API_PREFIX`], read by the secret
/// a short link carries: answered to anyone who names it, before they have a
/// device to enter with, as [`INVITATION_PATH`] answers whoever holds the
/// invitation itself. Its body and answer are JSON, as a directory's are.
pub const OPEN_INVITATION_PATH: &str = "/invitation/open";

/// What a short link asks its host for: the invitation `workspace` keeps
/// under `secret`, the link's `invite`
/// (`tilo_workspace_join::native::bearer_text`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptInvitation {
    /// The workspace's id, as the link names it.
    pub workspace: String,
    /// The secret the link carries.
    pub secret: String,
}

/// The invitation a short link names, as its workspace's host keeps it: the
/// invitation's own text, which every way of joining takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenedInvitation {
    pub invitation: String,
}

/// What a host answers for an invitation it keeps no more, or never kept:
/// the same for both, so a link tells nothing about invitations it does not
/// name.
pub const INVITATION_GONE: u16 = 410;

/// The host's fetch proxy, below [`API_PREFIX`].
pub const FETCH_PATH: &str = "/fetch";

/// Bound, in characters, of the display name a device asks a workspace's entry
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
