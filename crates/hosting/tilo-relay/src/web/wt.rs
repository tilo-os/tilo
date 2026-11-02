//! The page's live plane: the relay's WebTransport listener, and the authority
//! that admits a page's session token to it.
//!
//! A page holds no device certificate where its network stack runs, so it
//! proves itself with the session token its HTTPS handshake minted
//! ([`Sessions`]). The token arrives in the carrier's admission grant, and the
//! roster answer is made once, when the page asks for the live plane over
//! HTTPS: [`LiveInfo`] is that answer, so a page whose device the workspace does
//! not admit is refused there and never reaches the listener. The answer stays
//! current from the relay-wide authority epoch — a page's device removed from
//! the workspace is cut on that event, not on a timer.
//!
//! The sessions themselves are `tilo-live`'s: this listener only binds the
//! socket, presents the relay's certificate and hands each session to
//! [`Relay::serve`], which is the same admission preface, `moq-lite` session and
//! copying relay the native carrier reaches over iroh.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_live::live::Namespace;
use tilo_live::relay::{Denied, Direction, Relay, RelayAuthority, SessionAuthorization};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::web::{LiveGrant, LiveInfo};
use tokio::sync::watch;

use super::{Sessions, WebConfig, tls};
use crate::auth::AuthenticatedPeer;

/// One page's live session: whose it is, the scope it publishes in, and
/// whether the workspace still admits it.
struct Grant {
    token: [u8; 32],
    workspace: u64,
    peer: AuthenticatedPeer,
    /// The namespace the page publishes its own tracks under: one participant
    /// of the workspace scope, as every device's is.
    namespace: String,
    /// The subtree every participant of the same scope publishes in.
    scope: String,
    admitted: AtomicBool,
    /// Reports each change of `admitted`: the relay re-observes on it and cuts
    /// a session whose answer narrowed.
    changed: watch::Sender<u64>,
}

impl Grant {
    fn new(grant: LiveGrant, workspace_name: &str, peer: AuthenticatedPeer) -> Self {
        let namespace = Namespace::workspace(workspace_name, peer.device_public_key);
        Self {
            token: grant.token,
            workspace: grant.workspace_id,
            peer,
            scope: namespace.scope_path(),
            namespace: namespace.path(),
            admitted: AtomicBool::new(true),
            changed: watch::channel(0_u64).0,
        }
    }

    /// Whether the workspace admits this page now.
    fn admitted(&self) -> bool {
        self.admitted.load(Ordering::Acquire)
    }

    /// Record the current answer, waking the relay when it changes.
    fn set(&self, admitted: bool) {
        if self.admitted.swap(admitted, Ordering::AcqRel) != admitted {
            self.changed
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    }
}

impl SessionAuthorization for Grant {
    fn publication(&self) -> Option<&str> {
        Some(&self.namespace)
    }

    fn subscriptions(&self) -> &[String] {
        &[]
    }

    fn subscription_prefix(&self) -> Option<&str> {
        Some(&self.scope)
    }

    fn current(&self, direction: Direction, namespace: &str) -> bool {
        if !self.admitted() {
            return false;
        }
        match direction {
            Direction::Publish => namespace == self.namespace,
            Direction::Subscribe => namespace
                .strip_prefix(&self.scope)
                .is_some_and(|rest| rest.starts_with("/participant/")),
        }
    }

    fn changed(&self) -> Option<watch::Receiver<u64>> {
        Some(self.changed.subscribe())
    }
}

/// Who a grant is for: the page's session token, in one workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GrantKey {
    token: [u8; 32],
    workspace: u64,
}

impl From<LiveGrant> for GrantKey {
    fn from(grant: LiveGrant) -> Self {
        Self {
            token: grant.token,
            workspace: grant.workspace_id,
        }
    }
}

/// Every grant a page asked for, by the page and workspace it belongs to.
type Grants = HashMap<GrantKey, Arc<Grant>>;

/// Every page the live plane may admit: the grants asked for over HTTPS, and
/// the workspace's answer to each.
pub(crate) struct Pages {
    sessions: Arc<Sessions>,
    grants: Mutex<Grants>,
}

impl Pages {
    /// Record that the page `grant` names is admitted to its workspace now,
    /// creating its grant on the page's first ask and re-admitting it on a
    /// later one.
    pub(crate) fn issue(&self, grant: LiveGrant, workspace_name: &str, peer: &AuthenticatedPeer) {
        let mut grants = self.grants.lock_recover();
        match grants.get(&GrantKey::from(grant)) {
            Some(issued) => issued.set(true),
            None => {
                grants.insert(
                    GrantKey::from(grant),
                    Arc::new(Grant::new(grant, workspace_name, peer.clone())),
                );
            }
        }
    }

    /// Re-answer every granted page: is its device admitted to its workspace
    /// now, and is its HTTPS session still open? Called on each relay-wide
    /// authority change, never on a timer.
    async fn refresh(&self) {
        let granted: Vec<(GrantKey, Arc<Grant>)> = self
            .grants
            .lock_recover()
            .iter()
            .map(|(key, grant)| (*key, grant.clone()))
            .collect();
        let mut forgotten = Vec::new();
        for (key, grant) in granted {
            if self.sessions.get(&grant.token).is_err() {
                grant.set(false);
                forgotten.push(key);
                continue;
            }
            grant.set(self.sessions.admits(&grant.peer, grant.workspace).await);
        }
        if !forgotten.is_empty() {
            let mut grants = self.grants.lock_recover();
            for key in forgotten {
                grants.remove(&key);
            }
        }
    }
}

impl RelayAuthority for Pages {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let claim = LiveGrant::decode(proof).ok_or(Denied)?;
        let grant = self
            .grants
            .lock_recover()
            .get(&GrantKey::from(claim))
            .filter(|grant| {
                grant.admitted()
                    && self.sessions.get(&grant.token).is_ok_and(|session| {
                        session.peer.device_public_key == grant.peer.device_public_key
                    })
            })
            .cloned()
            .ok_or(Denied)?;
        Ok(grant)
    }
}

/// The relay's WebTransport listener: the page carrier's socket, the origin a
/// page dials, and the certificate it pins.
pub(crate) struct Live {
    pub(crate) info: LiveInfo,
    pub(crate) pages: Arc<Pages>,
    pub(crate) tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Live {
    /// Bind the live plane's socket beside the HTTPS listener — on `listen`'s
    /// address and port where that port is free on UDP — and serve the page
    /// carrier on it at `origin`'s host.
    ///
    /// # Errors
    ///
    /// Returns a message when the certificate is unusable or no socket can be
    /// bound: a relay serves no live plane it cannot reach.
    pub(crate) fn start(
        identity: &tls::Identity,
        sessions: Arc<Sessions>,
        config: &WebConfig,
        origin: &str,
    ) -> Result<Self, String> {
        let pages = Arc::new(Pages {
            sessions,
            grants: Mutex::new(HashMap::new()),
        });
        let (socket, port) = bind(config.listen)?;
        let endpoint = tilo_live::carrier::listener_on(socket, identity.certificate()?)
            .map_err(|error| format!("live listener: {error}"))?;
        let serve = Relay::new(pages.clone());
        let listener = tokio::spawn(async move { serve.serve(endpoint).await });
        let refreshing = pages.clone();
        let refresh = tokio::spawn(async move {
            let mut epoch = crate::authority_epoch::subscribe();
            while epoch.changed().await.is_ok() {
                refreshing.refresh().await;
            }
        });
        Ok(Self {
            info: LiveInfo {
                webtransport_port: port,
                certificate_sha256: certificate_sha256(identity)?,
                webtransport_origin: webtransport_origin(origin, port),
            },
            pages,
            tasks: vec![listener, refresh],
        })
    }
}

/// A UDP socket for the live listener. The HTTPS port is reused when it is
/// free on UDP, so one host answers on one port; otherwise the kernel picks
/// the port the page is told.
fn bind(listen: SocketAddr) -> Result<(std::net::UdpSocket, u16), String> {
    let address = SocketAddr::new(listen.ip(), listen.port());
    let socket = std::net::UdpSocket::bind(address)
        .or_else(|_| std::net::UdpSocket::bind(SocketAddr::new(listen.ip(), 0)))
        .map_err(|error| format!("live listener: {error}"))?;
    let port = socket
        .local_addr()
        .map_err(|error| format!("live listener: {error}"))?
        .port();
    Ok((socket, port))
}

/// Base64 SHA-256 of the listener's leaf certificate, as a page pins it in
/// `serverCertificateHashes`.
fn certificate_sha256(identity: &tls::Identity) -> Result<String, String> {
    use base64::Engine as _;
    use sha2::Digest as _;
    let leaf = identity
        .chain
        .first()
        .ok_or("live listener: the TLS identity presents no certificate")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(leaf.as_ref())))
}

/// The origin a page dials the live listener at: the relay's own origin's
/// host, on the live port.
fn webtransport_origin(origin: &str, port: u16) -> String {
    let host = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or(origin)
        .trim_end_matches('/');
    let host = match host.rsplit_once(':') {
        Some((host, named)) if named.bytes().all(|byte| byte.is_ascii_digit()) => host,
        _ => host,
    };
    format!("https://{host}:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_origin_is_the_relays_host_on_the_live_port() {
        assert_eq!(
            webtransport_origin("https://relay.example:8443", 8444),
            "https://relay.example:8444"
        );
        assert_eq!(
            webtransport_origin("https://relay.example", 8444),
            "https://relay.example:8444"
        );
        assert_eq!(
            webtransport_origin("http://localhost:3000/", 3001),
            "https://localhost:3001"
        );
    }
}
