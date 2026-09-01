//! Lease-scoped filtered egress for the one Windows guest.
//!
//! The appliance never receives a NAT interface.  Instead, a Windows-local
//! proxy connects to this host-only listener and authenticates with the
//! unguessable capability lease token below.  The relay checks the original
//! `NetHost` name before resolving it, so a guest application cannot turn a
//! name grant into arbitrary-address access. Dropping the lease closes the
//! listener; existing and new proxy connections then fail closed. This is a
//! host-side grant boundary, not a claim that arbitrary processes in the one
//! persistent Windows user session are mutually isolated.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use launcher::netgate::{Destination, EgressPolicy, connect_upstream};
use rand::RngCore as _;
use thiserror::Error;

/// qOS's immutable host-only gateway. This is not an internet route.
pub const HOST_ONLY_GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 251, 1);
const GUEST_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 251, 2);
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// Opaque proxy details carried only over the mutually-authenticated bridge.
/// The token is not a `NetHost` grant; it merely proves that the guest-local
/// proxy is using the already materialized short-lived lease.
#[derive(Clone, Eq, PartialEq)]
pub struct GuestNetworkBinding {
    pub endpoint: SocketAddr,
    pub token: String,
}

impl std::fmt::Debug for GuestNetworkBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuestNetworkBinding")
            .field("endpoint", &self.endpoint)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum GuestNetworkError {
    #[error("a filtered Windows network lease requires at least one NetHost")]
    EmptyAllowlist,
    #[error("the canonical NetHost value {0:?} is invalid")]
    InvalidHost(String),
    #[error("could not bind the host-only Windows egress relay: {0}")]
    Bind(#[source] io::Error),
    #[error("the guest network lease policy lock is unavailable")]
    PolicyUnavailable,
}

/// A real, token-authenticated CONNECT relay for one materialized lease.
///
/// It intentionally supports no SOCKS, UDP, BIND, or direct guest route. The
/// guest-side proxy translates normal HTTP proxy traffic to CONNECT, so the
/// host makes exactly one name-based decision before every TCP connection.
pub struct GuestNetworkProxy {
    binding: GuestNetworkBinding,
    shutdown: Arc<AtomicBool>,
    /// Duplicates held solely so revocation can interrupt already-established
    /// tunnels, not just stop accepting a new CONNECT request.
    active_connections: Arc<ActiveTunnels>,
}

impl GuestNetworkProxy {
    /// Start on the dedicated host-only address. Failure is a denied grant,
    /// never a reason to attach the appliance to a NAT network.
    pub fn start(hosts: Vec<String>) -> Result<Self, GuestNetworkError> {
        Self::start_at(
            SocketAddr::new(IpAddr::V4(HOST_ONLY_GATEWAY), 0),
            hosts,
            true,
        )
    }

    /// Injectable listener address for tests and service integration checks.
    pub fn start_at(
        address: SocketAddr,
        hosts: Vec<String>,
        require_guest_peer: bool,
    ) -> Result<Self, GuestNetworkError> {
        validate_hosts(&hosts)?;
        let listener = TcpListener::bind(address).map_err(GuestNetworkError::Bind)?;
        let endpoint = listener.local_addr().map_err(GuestNetworkError::Bind)?;
        let mut random = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let token = hex::encode(random);
        random.fill(0);
        let binding = GuestNetworkBinding { endpoint, token };
        let hosts = Arc::new(std::sync::Mutex::new(hosts));
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_connections = Arc::new(ActiveTunnels::default());
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_token = binding.token.clone();
        let worker_hosts = Arc::clone(&hosts);
        let worker_connections = Arc::clone(&active_connections);
        thread::spawn(move || {
            serve(
                listener,
                worker_hosts,
                worker_token,
                require_guest_peer,
                worker_shutdown,
                worker_connections,
            );
        });
        Ok(Self {
            binding,
            shutdown,
            active_connections,
        })
    }

    #[must_use]
    pub fn binding(&self) -> &GuestNetworkBinding {
        &self.binding
    }

    /// Tear down every tunnel immediately. This is idempotent and is called
    /// by both explicit lease revocation and `Drop`.
    pub fn revoke(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.active_connections.shutdown_all();
        // Wake `incoming` so the worker observes shutdown without waiting for
        // another guest request.
        let _ = TcpStream::connect(self.binding.endpoint);
    }
}

impl Drop for GuestNetworkProxy {
    fn drop(&mut self) {
        self.revoke();
    }
}

fn validate_hosts(hosts: &[String]) -> Result<(), GuestNetworkError> {
    if hosts.is_empty() {
        return Err(GuestNetworkError::EmptyAllowlist);
    }
    for host in hosts {
        if host.is_empty()
            || host.len() > 253
            || host
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(GuestNetworkError::InvalidHost(host.clone()));
        }
    }
    Ok(())
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the detached relay worker owns its listener and shared lease state"
)]
fn serve(
    listener: TcpListener,
    hosts: Arc<std::sync::Mutex<Vec<String>>>,
    token: String,
    require_guest_peer: bool,
    shutdown: Arc<AtomicBool>,
    active_connections: Arc<ActiveTunnels>,
) {
    for incoming in listener.incoming() {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let Ok(stream) = incoming else { continue };
        if require_guest_peer
            && !matches!(stream.peer_addr(), Ok(peer) if peer.ip() == IpAddr::V4(GUEST_ADDRESS))
        {
            continue;
        }
        let hosts = Arc::clone(&hosts);
        let token = token.clone();
        let shutdown = Arc::clone(&shutdown);
        let active_connections = Arc::clone(&active_connections);
        thread::spawn(move || {
            let _ = serve_connect(stream, &hosts, &token, &shutdown, &active_connections);
        });
    }
}

fn serve_connect(
    mut client: TcpStream,
    hosts: &std::sync::Mutex<Vec<String>>,
    token: &str,
    shutdown: &AtomicBool,
    active_connections: &ActiveTunnels,
) -> io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    client.set_write_timeout(Some(Duration::from_secs(30)))?;
    let head = read_head(&mut client)?;
    let Some((host, port, supplied_token)) = parse_connect(&head) else {
        return write_response(&mut client, "400 Bad Request");
    };
    if supplied_token != token {
        return write_response(&mut client, "502 Bad Gateway");
    }
    // Do not resolve here. `connect_upstream` is the canonical qOS NetHost
    // path: it resolves after policy, rejects address-literal bypasses, and
    // removes loopback/link-local/this-host addresses unless named exactly.
    let policy = EgressPolicy::new(
        hosts
            .lock()
            .map_err(|_| io::Error::other("network lease policy unavailable"))?
            .clone(),
    );
    if !policy.permits(&host) {
        return write_response(&mut client, "502 Bad Gateway");
    }
    let Ok(mut upstream) = connect_upstream(&Destination::Domain(host, port), &policy) else {
        return write_response(&mut client, "502 Bad Gateway");
    };
    if shutdown.load(Ordering::Acquire) {
        return write_response(&mut client, "502 Bad Gateway");
    }
    // Keep independent descriptor references in the lease.  Closing these on
    // revoke causes both halves of the bidirectional copy to unblock.
    let client_control = client.try_clone()?;
    let upstream_control = upstream.try_clone()?;
    let _tunnel = active_connections.register(client_control, upstream_control, shutdown)?;
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    client.set_read_timeout(None)?;
    client.set_write_timeout(None)?;
    upstream.set_read_timeout(None)?;
    upstream.set_write_timeout(None)?;
    let mut reader = client.try_clone()?;
    let mut upstream_writer = upstream.try_clone()?;
    let copy = thread::spawn(move || io::copy(&mut reader, &mut upstream_writer));
    let _ = io::copy(&mut upstream, &mut client);
    let _ = copy.join();
    Ok(())
}

#[derive(Default)]
struct ActiveTunnels {
    next: AtomicU64,
    tunnels: Mutex<HashMap<u64, [TcpStream; 2]>>,
}

impl ActiveTunnels {
    fn register(
        &self,
        client: TcpStream,
        upstream: TcpStream,
        shutdown: &AtomicBool,
    ) -> io::Result<TunnelRegistration<'_>> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut tunnels = self
            .tunnels
            .lock()
            .map_err(|_| io::Error::other("network lease connection state unavailable"))?;
        // Registration and `shutdown_all` use the same mutex. If revocation
        // wins first, refuse the tunnel; if registration wins first, revoke
        // necessarily observes and closes it. This removes the check/insert
        // gap that could otherwise leave one post-revoke CONNECT alive.
        if shutdown.load(Ordering::Acquire) {
            return Err(io::Error::other("network lease was revoked"));
        }
        tunnels.insert(id, [client, upstream]);
        drop(tunnels);
        Ok(TunnelRegistration { tunnels: self, id })
    }

    fn shutdown_all(&self) {
        if let Ok(tunnels) = self.tunnels.lock() {
            for tunnel in tunnels.values().flatten() {
                let _ = tunnel.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.tunnels.lock().map_or(0, |tunnels| tunnels.len())
    }
}

/// An active CONNECT tunnel is removed even when either copy path errors or
/// panics while unwinding. The retained descriptors exist only for revocation.
struct TunnelRegistration<'a> {
    tunnels: &'a ActiveTunnels,
    id: u64,
}

impl Drop for TunnelRegistration<'_> {
    fn drop(&mut self) {
        if let Ok(mut tunnels) = self.tunnels.tunnels.lock() {
            tunnels.remove(&self.id);
        }
    }
}

fn read_head(stream: &mut TcpStream) -> io::Result<String> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    while bytes.len() < MAX_HEAD_BYTES {
        stream.read_exact(&mut byte)?;
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            return String::from_utf8(bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF8 proxy head"));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "oversized proxy head",
    ))
}

fn parse_connect(head: &str) -> Option<(String, u16, String)> {
    let mut lines = head.split("\r\n");
    let mut parts = lines.next()?.split_ascii_whitespace();
    if !parts.next()?.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    let authority = parts.next()?;
    parts.next()?;
    let (host, port) = authority.rsplit_once(':')?;
    let port = port.parse().ok()?;
    if host.is_empty() || host.len() > 253 || host.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return None;
    }
    let token = lines.find_map(|line| line.strip_prefix("Proxy-Authorization: Bearer "))?;
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some((host.to_ascii_lowercase(), port, token.to_string()))
}

fn write_response(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_or_unsafe_canonical_nethosts() {
        assert!(matches!(
            GuestNetworkProxy::start_at("127.0.0.1:0".parse().unwrap(), Vec::new(), false),
            Err(GuestNetworkError::EmptyAllowlist)
        ));
        assert!(matches!(
            GuestNetworkProxy::start_at(
                "127.0.0.1:0".parse().unwrap(),
                vec!["bad host".into()],
                false
            ),
            Err(GuestNetworkError::InvalidHost(_))
        ));
    }

    #[test]
    fn binding_never_exposes_its_bearer_token_through_debug() {
        let proxy = GuestNetworkProxy::start_at(
            "127.0.0.1:0".parse().unwrap(),
            vec!["example.test".into()],
            false,
        )
        .unwrap();
        assert!(!format!("{:?}", proxy.binding()).contains(&proxy.binding().token));
        assert_eq!(
            proxy.binding().endpoint.ip(),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
    }

    #[test]
    fn parser_requires_connect_and_a_full_lease_token() {
        assert!(
            parse_connect(
                "CONNECT example.test:443 HTTP/1.1\r\nProxy-Authorization: Bearer aaaa\r\n\r\n"
            )
            .is_none()
        );
        assert_eq!(
            parse_connect(&format!(
                "CONNECT example.test:443 HTTP/1.1\r\nProxy-Authorization: Bearer {}\r\n\r\n",
                "a".repeat(64)
            ))
            .map(|value| value.0),
            Some("example.test".into())
        );
    }

    fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn active_tunnel_registration_is_removed_when_the_copy_finishes() {
        let tunnels = ActiveTunnels::default();
        let (client, server) = connected_pair();
        let shutdown = AtomicBool::new(false);
        let registration = tunnels.register(client, server, &shutdown).unwrap();
        assert_eq!(tunnels.len(), 1);
        drop(registration);
        assert_eq!(tunnels.len(), 0, "completed tunnels retain no descriptors");
    }

    #[test]
    fn revocation_interrupts_an_active_tunnel_immediately() {
        let proxy = GuestNetworkProxy::start_at(
            "127.0.0.1:0".parse().unwrap(),
            vec!["example.test".into()],
            false,
        )
        .unwrap();
        let (client, mut peer) = connected_pair();
        let _registration = proxy
            .active_connections
            .register(client, peer.try_clone().unwrap(), &proxy.shutdown)
            .unwrap();
        proxy.revoke();
        peer.set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut byte = [0_u8; 1];
        match peer.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                ) => {}
            result => panic!("active tunnel survived revoke: {result:?}"),
        }
    }

    #[test]
    fn revoked_lease_rejects_late_tunnel_registration() {
        let tunnels = ActiveTunnels::default();
        let shutdown = AtomicBool::new(true);
        let (client, server) = connected_pair();
        assert!(tunnels.register(client, server, &shutdown).is_err());
        assert_eq!(tunnels.len(), 0);
    }
}
