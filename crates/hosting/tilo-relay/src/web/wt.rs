//! The relay's live plane: one `moq-lite` relay every carrier of a workspace's
//! live tracks reaches, and the authority that admits them to it.
//!
//! The plane belongs to the relay, not to a listener: a device dials it over
//! the relay's iroh endpoint ([`tilo_live::native`]) as its own certificate
//! says it may, and a page dials it over WebTransport with the session token
//! its HTTPS handshake minted ([`Sessions`]). Both land in one origin, so a
//! desk and a page in the same workspace hear each other whatever carrier each
//! speaks.
//!
//! A page holds no device certificate where its network stack runs, so it
//! proves itself with its session token: the roster answer is made once, when
//! the page asks for the live plane over HTTPS ([`LiveInfo`]), so a page whose
//! device the workspace does not admit is refused there and never reaches the
//! listener. A device over iroh presents the certificate its grant carries, and
//! the workspace's governance answers for it as it answers for any member.
//! Either answer stays current from the relay-wide authority epoch — a device
//! removed from the workspace is cut on that event, not on a timer.
//!
//! The sessions themselves are `tilo-live`'s: this listener only binds the
//! socket, presents the relay's certificate and hands each session to
//! [`Relay::serve`], which is the same admission preface, `moq-lite` session
//! and copying relay the native carrier reaches over iroh.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_live::live::Namespace;
use tilo_live::relay::{Denied, Direction, Peer, Relay, RelayAuthority, SessionAuthorization};
use tilo_live::scope::Scope;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::web::{LiveGrant, LiveInfo};
use tokio::sync::watch;

use super::{Sessions, WebConfig, tls};
use crate::auth::AuthenticatedPeer;
use crate::host::RelayProtocol;
use crate::workspace_access::{self, WorkspaceOperation};

/// One participant's live session: whose it is, the scope it publishes in, and
/// whether the workspace still admits it.
struct Grant {
    token: [u8; 32],
    workspace: u64,
    peer: AuthenticatedPeer,
    /// The namespace the participant publishes its own tracks under: one
    /// participant of the workspace scope, as every device's is.
    namespace: String,
    /// The subtree every participant of the same scope publishes in.
    scope: String,
    admitted: AtomicBool,
    /// Reports each change of `admitted`: the relay re-observes on it and cuts
    /// a session whose answer narrowed.
    changed: watch::Sender<u64>,
}

impl Grant {
    fn new(token: [u8; 32], workspace: u64, workspace_name: &str, peer: AuthenticatedPeer) -> Self {
        let namespace = Namespace::new(&Scope::workspace(workspace_name), peer.device_public_key);
        Self {
            token,
            workspace,
            peer,
            scope: namespace.scope_path(),
            namespace: namespace.path(),
            admitted: AtomicBool::new(true),
            changed: watch::channel(0_u64).0,
        }
    }

    /// Whether the workspace admits this participant now.
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
        self.admitted()
            && match direction {
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

/// Who a page's grant is for: the page's session token, in one workspace.
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

/// Who a device's grant is for: the device, in one workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct DeviceKey {
    device: [u8; 32],
    workspace: u64,
}

/// Every page grant a page asked for, by the page and workspace it belongs to.
type Grants = HashMap<GrantKey, Arc<Grant>>;

/// Every device the plane admits over iroh, by the device and workspace it
/// presented.
type Members = BTreeMap<DeviceKey, Arc<Grant>>;

/// The live plane's authority: the pages the relay's HTTPS routes admitted and
/// the devices the workspaces' governance admits.
pub(crate) struct Pages {
    relay: RelayProtocol,
    /// The browser sessions of this relay, once it serves browsers: a page's
    /// session must still be open for its grant to stand.
    sessions: Mutex<Option<Arc<Sessions>>>,
    grants: Mutex<Grants>,
    members: Mutex<Members>,
}

impl Pages {
    fn new(relay: RelayProtocol) -> Self {
        Self {
            relay,
            sessions: Mutex::new(None),
            grants: Mutex::new(HashMap::new()),
            members: Mutex::new(Members::new()),
        }
    }

    /// Serve browsers from now on: a page's grant stands while its session
    /// with `sessions` does.
    pub(crate) fn serve_pages(&self, sessions: Arc<Sessions>) {
        *self.sessions.lock_recover() = Some(sessions);
    }

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
                    Arc::new(Grant::new(
                        grant.token,
                        grant.workspace_id,
                        workspace_name,
                        peer.clone(),
                    )),
                );
            }
        }
    }

    /// Who the plane carries now: every person with an admitted participant,
    /// with the devices they are here on. One person on two devices is one
    /// entry. A plane is the one account of who is here: a native device and a
    /// page alike reach it, and nothing counts a page that never joined.
    pub(crate) fn participants(&self) -> BTreeMap<[u8; 32], BTreeSet<[u8; 32]>> {
        let mut people: BTreeMap<[u8; 32], BTreeSet<[u8; 32]>> = BTreeMap::new();
        for grant in self.carried() {
            if grant.admitted() {
                people
                    .entry(grant.peer.user_public_key)
                    .or_default()
                    .insert(grant.peer.device_public_key);
            }
        }
        people
    }

    /// Every grant the plane carries: a host hands out its own and its
    /// members' alike, and both count as here once admitted.
    fn carried(&self) -> Vec<Arc<Grant>> {
        let grants = self.grants.lock_recover();
        let members = self.members.lock_recover();
        grants.values().chain(members.values()).cloned().collect()
    }

    /// The devices the plane carries in `workspace` now, each with the person
    /// it is certified as: who a page there pairs with before anyone speaks.
    pub(crate) fn persons_in(&self, workspace: u64) -> Vec<([u8; 32], [u8; 32])> {
        let mut persons: Vec<([u8; 32], [u8; 32])> = self
            .carried()
            .into_iter()
            .filter(|grant| grant.admitted() && grant.workspace == workspace)
            .map(|grant| (grant.peer.device_public_key, grant.peer.user_public_key))
            .collect();
        persons.sort_unstable();
        persons.dedup();
        persons
    }

    /// Re-answer every granted participant: is its device admitted to its
    /// workspace now, and (for a page) is its HTTPS session still open? Called
    /// on each relay-wide authority change, never on a timer.
    async fn refresh(&self) {
        let sessions = self.sessions.lock_recover().clone();
        let granted: Vec<(GrantKey, Arc<Grant>)> = self
            .grants
            .lock_recover()
            .iter()
            .map(|(key, grant)| (*key, grant.clone()))
            .collect();
        if let Some(sessions) = &sessions {
            let mut forgotten = Vec::new();
            for (key, grant) in granted {
                if sessions.get(&grant.token).is_err() {
                    grant.set(false);
                    forgotten.push(key);
                    continue;
                }
                grant.set(sessions.admits(&grant.peer, grant.workspace).await);
            }
            if !forgotten.is_empty() {
                let mut grants = self.grants.lock_recover();
                for key in forgotten {
                    grants.remove(&key);
                }
            }
        }
        let members: Vec<Arc<Grant>> = self.members.lock_recover().values().cloned().collect();
        for grant in members {
            grant.set(self.admits_member(&grant.peer, grant.workspace));
        }
    }

    /// Whether the workspace's governance admits `peer`'s device to take its
    /// own frames into the workspace now: the same answer every plane of the
    /// workspace takes, read from the workspace as it stands.
    fn admits_member(&self, peer: &AuthenticatedPeer, workspace: u64) -> bool {
        let Ok(workspaces) = self.relay.workspaces.try_lock() else {
            return false;
        };
        let Some(workspace) = workspaces.get(&workspace).cloned() else {
            return false;
        };
        drop(workspaces);
        let Ok(runtime) = workspace.try_lock() else {
            return false;
        };
        workspace_access::decide(
            &runtime,
            &self.relay.trust(),
            peer,
            WorkspaceOperation::Publish,
        )
        .admitted()
    }

    /// Admit the page behind `proof`'s session token, while that session is
    /// open and the workspace admits its device.
    fn admit_page(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let claim = LiveGrant::decode(proof).ok_or(Denied)?;
        let sessions = self.sessions.lock_recover().clone();
        let grant = self
            .grants
            .lock_recover()
            .get(&GrantKey::from(claim))
            .filter(|grant| {
                grant.admitted()
                    && sessions.as_ref().is_some_and(|sessions| {
                        sessions.get(&grant.token).is_ok_and(|session| {
                            session.peer.device_public_key == grant.peer.device_public_key
                        })
                    })
            })
            .cloned()
            .ok_or(Denied)?;
        Ok(grant)
    }

    /// Admit the device that reached an iroh connection, which proves itself
    /// with the certificate its grant carries.
    fn admit_member(
        &self,
        endpoint: [u8; 32],
        proof: &[u8],
    ) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let grant = tilo_live::grant::Native::parse(proof).ok_or(Denied)?;
        let workspace = tilo_live::live::workspace_id(grant.workspace);
        let id = tilo_relay_protocol::room_workspace_id(&workspace).map_err(|_| Denied)?;
        if grant.scope != Scope::workspace(&workspace).id() {
            return Err(Denied);
        }
        // The certificate binds the handshake's key to the device: a device
        // presenting another device's certificate is refused.
        let certificate: tilo_user_identity::DeviceCert =
            serde_json::from_slice(&grant.certificate).map_err(|_| Denied)?;
        // The endpoint speaks for that device: it is the device's own key, or
        // one the device signed a live delegation for — a page's key cannot
        // leave its custody, so its endpoint speaks under a key of its own.
        if endpoint != certificate.device_public_key {
            let delegation: tilo_user_identity::EndpointDelegation =
                serde_json::from_slice(&grant.delegation).map_err(|_| Denied)?;
            if delegation.device_public_key != certificate.device_public_key {
                return Err(Denied);
            }
            delegation
                .verify(&endpoint, tilo_clock::unix_now_ns())
                .map_err(|_| Denied)?;
        }
        let device = certificate.device_public_key;
        if certificate.is_expired_at(tilo_clock::unix_now_ns()) {
            return Err(Denied);
        }
        let peer = AuthenticatedPeer {
            user_public_key: certificate.user_public_key,
            device_public_key: device,
            cert: certificate,
        };
        if !self.admits_member(&peer, id) {
            return Err(Denied);
        }
        let key = DeviceKey {
            device,
            workspace: id,
        };
        let admitted = self.members.lock_recover().get(&key).cloned();
        let member = if let Some(member) = admitted {
            member.set(true);
            member
        } else {
            let member = Arc::new(Grant::new([0; 32], id, &workspace, peer));
            self.members.lock_recover().insert(key, member.clone());
            member
        };
        Ok(member)
    }
}

impl RelayAuthority for Pages {
    fn admit(&self, peer: Peer, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        // A page's grant is its session token; a device that reached an iroh
        // connection proves itself with its certificate instead.
        let Peer::Native(device) = peer else {
            return self.admit_page(proof);
        };
        self.admit_member(device, proof)
    }
}

/// The relay's live plane: the origin every carrier of its workspaces'
/// tracks reaches, and the authority that admits them.
pub(crate) struct Plane {
    pub(crate) pages: Arc<Pages>,
    relay: Relay,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Plane {
    /// Start the plane for `relay`'s workspace: the origin its sessions copy
    /// into, and the answer that cuts an admitted one when the workspace moves
    /// its membership. Must be called inside a Tokio runtime.
    pub(crate) fn start(relay: RelayProtocol) -> Self {
        let pages = Arc::new(Pages::new(relay));
        let plane = Relay::new(pages.clone());
        let refreshing = pages.clone();
        let refresh = tokio::spawn(async move {
            let mut epoch = crate::authority_epoch::subscribe();
            while epoch.changed().await.is_ok() {
                refreshing.refresh().await;
            }
        });
        Self {
            pages,
            relay: plane,
            tasks: vec![refresh],
        }
    }

    /// The protocol a device dials this plane on: one `moq-lite` session on
    /// the iroh carrier, admitted by the device's grant as any other carrier
    /// session is.
    pub(crate) fn protocol(&self) -> (Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>) {
        let relay = self.relay.clone();
        let handler = tilo_live::native::handler(move |session: tilo_live::native::Session| {
            let relay = relay.clone();
            async move {
                let peer = Peer::Native(session.remote());
                relay
                    .serve_session(tilo_live::relay::VoiceRoute::negotiated(), session, peer)
                    .await;
            }
        });
        (tilo_live::native::ALPN.to_vec(), Box::new(handler))
    }

    /// The one task the plane runs besides its sessions, which stands while
    /// the plane does.
    pub(crate) fn tasks(&mut self) -> Vec<tokio::task::JoinHandle<()>> {
        std::mem::take(&mut self.tasks)
    }
}

/// The relay's WebTransport listener: the page carrier's socket, the origin a
/// page dials, and the certificate it pins.
pub(crate) struct Live {
    pub(crate) info: LiveInfo,
    /// The UDP socket the plane answers on.
    pub(crate) bound: SocketAddr,
    pub(crate) pages: Arc<Pages>,
    pub(crate) tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Live {
    /// Bind the live plane's socket — on `--web-live` when a proxy forwards to
    /// it, else beside the HTTPS listener on its address and port where that
    /// port is free on UDP — and serve the page carrier on it, announced at
    /// `--web-live-origin` when given, else at `origin`'s host on the bound
    /// port.
    ///
    /// # Errors
    ///
    /// Returns a message when the certificate is unusable, the announced
    /// origin is not an `https://host[:port]` origin or is a host the
    /// certificate does not name, or no socket can be bound: a relay serves no
    /// live plane it cannot reach.
    pub(crate) fn start(
        plane: &Plane,
        identity: &tls::Identity,
        config: &WebConfig,
        origin: &str,
    ) -> Result<Self, String> {
        let (socket, bound) = bind(config.live.listen, config.listen)?;
        let endpoint = tilo_live::carrier::listener_on(socket, identity.certificate()?)
            .map_err(|error| format!("live listener: {error}"))?;
        let (webtransport_origin, port) = match config.live.origin.as_deref() {
            Some(configured) => live_origin(configured)?,
            None => (webtransport_origin(origin, bound.port()), bound.port()),
        };
        unnamed_origin(identity, &webtransport_origin)?;
        let serve = plane.relay.clone();
        let listener = tokio::spawn(async move { serve.serve(endpoint).await });
        Ok(Self {
            info: LiveInfo {
                webtransport_port: port,
                certificate_sha256: certificate_sha256(identity)?,
                webtransport_origin,
            },
            bound,
            pages: plane.pages.clone(),
            tasks: vec![listener],
        })
    }
}

/// A UDP socket for the live listener. A configured address is the one a proxy
/// forwards to, so the socket binds exactly there or the host stops. By
/// default the HTTPS port is reused when it is free on UDP, so one host
/// answers on one port; otherwise the kernel picks the port the page is told.
fn bind(
    configured: Option<SocketAddr>,
    listen: SocketAddr,
) -> Result<(std::net::UdpSocket, SocketAddr), String> {
    let socket = match configured {
        Some(address) => std::net::UdpSocket::bind(address),
        None => std::net::UdpSocket::bind(SocketAddr::new(listen.ip(), listen.port()))
            .or_else(|_| std::net::UdpSocket::bind(SocketAddr::new(listen.ip(), 0))),
    }
    .map_err(|error| format!("live listener: {error}"))?;
    let bound = socket
        .local_addr()
        .map_err(|error| format!("live listener: {error}"))?;
    Ok((socket, bound))
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

/// A configured `https://host[:port]` origin pages dial the live listener at,
/// and the port it names: 443 when it names none.
fn live_origin(text: &str) -> Result<(String, u16), String> {
    let refuse = || format!("live origin: {text}: expected https://host[:port]");
    let uri: hyper::Uri = text.trim().parse().map_err(|_| refuse())?;
    let authority = uri.authority().ok_or_else(refuse)?;
    if uri.scheme_str() != Some("https")
        || !matches!(uri.path(), "" | "/")
        || uri.query().is_some()
        || authority.as_str().contains('@')
    {
        return Err(refuse());
    }
    let origin = format!("https://{}", authority.as_str());
    Ok((origin, uri.port_u16().unwrap_or(443)))
}

/// Refuses an origin the certificate does not name: a page pins the
/// certificate for the origin it is told, and a client that reads the names as
/// well finds none. A certificate from files carries names this host does not
/// read, so it is not judged here.
fn unnamed_origin(identity: &tls::Identity, origin: &str) -> Result<(), String> {
    let Some(named) = identity.hosts.as_deref() else {
        return Ok(());
    };
    let host = origin
        .strip_prefix("https://")
        .unwrap_or(origin)
        .split([':', '/'])
        .next()
        .unwrap_or_default();
    if named.iter().any(|name| name.eq_ignore_ascii_case(host)) {
        return Ok(());
    }
    Err(format!(
        "live origin {origin}: the certificate names {named:?}, not {host}: publish that host \
         with --public-host, or announce an origin this certificate names"
    ))
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

    #[test]
    fn a_configured_live_origin_is_announced_as_it_is_and_names_the_port() {
        assert_eq!(
            live_origin("https://live.eu1.tilo.coffee").unwrap(),
            ("https://live.eu1.tilo.coffee".into(), 443)
        );
        assert_eq!(
            live_origin("https://relay.example:9443/").unwrap(),
            ("https://relay.example:9443".into(), 9443)
        );
        // Not an origin a page can dial: no scheme, not https, or a path.
        assert!(live_origin("relay.example:9443").is_err());
        assert!(live_origin("http://relay.example").is_err());
        assert!(live_origin("https://relay.example/live").is_err());
        assert!(live_origin("https://").is_err());
    }
}
