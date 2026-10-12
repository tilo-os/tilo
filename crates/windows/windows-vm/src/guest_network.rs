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
//!
//! Connections are tasks on the process's shared egress runtime
//! ([`launcher::netgate::egress_runtime`]), at most
//! [`launcher::netgate::MAX_CONNECTIONS`] at once; past that the listener
//! stops accepting and further guest connections wait in its backlog.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use getrandom::{SysRng, rand_core::UnwrapErr};
use launcher::netgate::{Destination, EgressPolicy, NetHostPattern, connect_upstream_async};
use rand::Rng as _;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, Semaphore};

/// Tilo's immutable host-only gateway. This is not an internet route.
pub const HOST_ONLY_GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 251, 1);
const GUEST_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 251, 2);
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// How long the guest proxy gets for each read and write of a request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

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
    #[error("could not start the runtime serving the Windows egress relay: {0}")]
    Runtime(#[source] io::Error),
    #[error("the guest network lease policy lock is unavailable")]
    PolicyUnavailable,
}

/// A token-authenticated CONNECT relay for one materialized lease.
///
/// It intentionally supports no SOCKS, UDP, BIND, or direct guest route. The
/// guest-side proxy translates normal HTTP proxy traffic to CONNECT, so the
/// host makes exactly one name-based decision before every TCP connection.
pub struct GuestNetworkProxy {
    binding: GuestNetworkBinding,
    shutdown: Arc<AtomicBool>,
    /// Wakes the accept loop so it observes `shutdown` and closes the
    /// listener without waiting for another guest request.
    stop: Arc<Notify>,
    /// Duplicates held solely so revocation can interrupt already-established
    /// tunnels, not just stop accepting a new CONNECT request.
    active_connections: Arc<ActiveTunnels>,
}

impl GuestNetworkProxy {
    /// Start on the dedicated host-only address. Failure is a denied grant,
    /// never a reason to attach the appliance to a NAT network.
    pub fn start(hosts: &[String]) -> Result<Self, GuestNetworkError> {
        Self::start_at(
            SocketAddr::new(IpAddr::V4(HOST_ONLY_GATEWAY), 0),
            hosts,
            true,
        )
    }

    /// Injectable listener address for tests and service integration checks.
    pub fn start_at(
        address: SocketAddr,
        hosts: &[String],
        require_guest_peer: bool,
    ) -> Result<Self, GuestNetworkError> {
        Self::start_limited(
            address,
            hosts,
            require_guest_peer,
            launcher::netgate::MAX_CONNECTIONS,
        )
    }

    fn start_limited(
        address: SocketAddr,
        hosts: &[String],
        require_guest_peer: bool,
        max_connections: usize,
    ) -> Result<Self, GuestNetworkError> {
        let policy = lease_policy(hosts)?;
        let listener = std::net::TcpListener::bind(address).map_err(GuestNetworkError::Bind)?;
        let endpoint = listener.local_addr().map_err(GuestNetworkError::Bind)?;
        listener
            .set_nonblocking(true)
            .map_err(GuestNetworkError::Bind)?;
        let runtime = launcher::netgate::egress_runtime().map_err(GuestNetworkError::Runtime)?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener).map_err(GuestNetworkError::Bind)?
        };
        let mut random = [0_u8; 32];
        UnwrapErr(SysRng).fill_bytes(&mut random);
        let token = hex::encode(random);
        random.fill(0);
        let binding = GuestNetworkBinding { endpoint, token };
        let lease = Arc::new(Lease {
            policy,
            token: binding.token.clone(),
            shutdown: Arc::new(AtomicBool::new(false)),
            active_connections: Arc::new(ActiveTunnels::default()),
        });
        let stop = Arc::new(Notify::new());
        let proxy = Self {
            binding,
            shutdown: Arc::clone(&lease.shutdown),
            stop: Arc::clone(&stop),
            active_connections: Arc::clone(&lease.active_connections),
        };
        runtime.spawn(serve(
            listener,
            lease,
            require_guest_peer,
            max_connections,
            stop,
        ));
        Ok(proxy)
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
        // Stored as a permit if the accept loop is not waiting right now,
        // so it cannot miss the wake-up.
        self.stop.notify_one();
    }
}

impl Drop for GuestNetworkProxy {
    fn drop(&mut self) {
        self.revoke();
    }
}

/// The lease's policy, read through the one `NetHost` grammar
/// (`collab_types::caps::NetHostPattern`) every other enforcer reads. A host
/// that does not parse — an IP literal included — refuses the whole lease
/// rather than silently narrowing it: the caller asked for exact canonical
/// values.
fn lease_policy(hosts: &[String]) -> Result<EgressPolicy, GuestNetworkError> {
    if hosts.is_empty() {
        return Err(GuestNetworkError::EmptyAllowlist);
    }
    if let Some(bad) = hosts
        .iter()
        .find(|host| NetHostPattern::parse(host).is_err())
    {
        return Err(GuestNetworkError::InvalidHost(bad.clone()));
    }
    Ok(EgressPolicy::from_grants(hosts.iter().map(String::as_str)))
}

/// What every connection of one lease shares.
struct Lease {
    policy: EgressPolicy,
    token: String,
    shutdown: Arc<AtomicBool>,
    active_connections: Arc<ActiveTunnels>,
}

/// Accept guest connections until revoked, each served as its own task, at
/// most `limit` at once. A slot is claimed before accepting, so at the limit
/// the loop stops accepting and new connections wait in the listen backlog.
async fn serve(
    listener: tokio::net::TcpListener,
    lease: Arc<Lease>,
    require_guest_peer: bool,
    limit: usize,
    stop: Arc<Notify>,
) {
    let slots = Arc::new(Semaphore::new(limit.max(1)));
    loop {
        let slot = tokio::select! {
            () = stop.notified() => return,
            slot = Arc::clone(&slots).acquire_owned() => match slot {
                Ok(slot) => slot,
                Err(_) => return,
            },
        };
        let accepted = tokio::select! {
            () = stop.notified() => return,
            accepted = listener.accept() => accepted,
        };
        if lease.shutdown.load(Ordering::Acquire) {
            return;
        }
        let Ok((stream, peer)) = accepted else {
            continue;
        };
        if require_guest_peer && peer.ip() != IpAddr::V4(GUEST_ADDRESS) {
            continue;
        }
        let lease = Arc::clone(&lease);
        tokio::spawn(async move {
            let _ = serve_connect(stream, &lease).await;
            drop(slot);
        });
    }
}

/// Bound one read or write of the request head by [`HEAD_TIMEOUT`].
async fn within_head_timeout<T>(io: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::time::timeout(HEAD_TIMEOUT, io)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "proxy request timed out"))?
}

async fn serve_connect(mut client: tokio::net::TcpStream, lease: &Lease) -> io::Result<()> {
    let (head, early) = read_head(&mut client).await?;
    let Some((host, port, supplied_token)) = parse_connect(&head) else {
        return write_response(&mut client, "400 Bad Request").await;
    };
    if supplied_token != lease.token {
        return write_response(&mut client, "502 Bad Gateway").await;
    }
    // Do not resolve here. `connect_upstream_async` is the canonical Tilo
    // NetHost path: it resolves after policy, rejects address-literal
    // bypasses, and removes loopback/link-local/this-host addresses unless
    // named exactly.
    let policy = &lease.policy;
    if !policy.permits(&host) {
        return write_response(&mut client, "502 Bad Gateway").await;
    }
    let Ok(upstream) = connect_upstream_async(&Destination::Domain(host, port), policy).await
    else {
        return write_response(&mut client, "502 Bad Gateway").await;
    };
    if lease.shutdown.load(Ordering::Acquire) {
        return write_response(&mut client, "502 Bad Gateway").await;
    }
    // Keep independent descriptor references in the lease.  Shutting these
    // down on revoke ends both directions of the copy below.
    let client = client.into_std()?;
    let upstream = upstream.into_std()?;
    let _tunnel = lease.active_connections.register(
        client.try_clone()?,
        upstream.try_clone()?,
        &lease.shutdown,
    )?;
    let mut client = tokio::net::TcpStream::from_std(client)?;
    let mut upstream = tokio::net::TcpStream::from_std(upstream)?;
    within_head_timeout(client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")).await?;
    // Bytes the guest proxy sent right behind the head belong to the tunnel.
    if !early.is_empty() {
        upstream.write_all(&early).await?;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
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

/// Read a request head, up to and including the blank line. Returns the
/// head and whatever arrived after it.
async fn read_head(stream: &mut tokio::net::TcpStream) -> io::Result<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let early = bytes.split_off(end.saturating_add(4));
            let head = String::from_utf8(bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF8 proxy head"))?;
            return Ok((head, early));
        }
        if bytes.len() >= MAX_HEAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized proxy head",
            ));
        }
        let n = within_head_timeout(stream.read(&mut chunk)).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "proxy head ended early",
            ));
        }
        bytes.extend_from_slice(chunk.get(..n).unwrap_or_default());
    }
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

async fn write_response(stream: &mut tokio::net::TcpStream, status: &str) -> io::Result<()> {
    within_head_timeout(
        stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes()),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn guest_network_leases_parse_strictly_and_hide_their_tokens() {
        // Phase `rejects_empty_or_unsafe_canonical_nethosts`.
        {
            assert!(matches!(
                GuestNetworkProxy::start_at("127.0.0.1:0".parse().unwrap(), &[], false),
                Err(GuestNetworkError::EmptyAllowlist)
            ));
            assert!(matches!(
                GuestNetworkProxy::start_at(
                    "127.0.0.1:0".parse().unwrap(),
                    &["bad host".into()],
                    false
                ),
                Err(GuestNetworkError::InvalidHost(_))
            ));
        }
        // Phase `reads_hosts_through_the_one_nethost_grammar`.
        {
            // An address is not a host, for the guest as for apps and
            // environments.
            for address in ["10.0.0.5", "[::1]", "192.168.1.1"] {
                assert!(
                    matches!(
                        GuestNetworkProxy::start_at(
                            "127.0.0.1:0".parse().unwrap(),
                            &[address.into()],
                            false
                        ),
                        Err(GuestNetworkError::InvalidHost(ref host)) if host == address
                    ),
                    "{address} must not be leased"
                );
            }
            // The domain spelling apps and environments accept is accepted.
            assert_eq!(
                lease_policy(&["*.example.test".into()]).unwrap(),
                EgressPolicy::from_grants([".example.test"])
            );
        }
        // Phase `binding_never_exposes_its_bearer_token_through_debug`.
        {
            let proxy = GuestNetworkProxy::start_at(
                "127.0.0.1:0".parse().unwrap(),
                &["example.test".into()],
                false,
            )
            .unwrap();
            assert!(!format!("{:?}", proxy.binding()).contains(&proxy.binding().token));
            assert_eq!(
                proxy.binding().endpoint.ip(),
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            );
        }
        // Phase `parser_requires_connect_and_a_full_lease_token`.
        {
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
    }

    fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn tunnels_register_carry_bytes_and_close_on_revocation() {
        // Phase `active_tunnel_registration_is_removed_when_the_copy_finishes`.
        {
            let tunnels = ActiveTunnels::default();
            let (client, server) = connected_pair();
            let shutdown = AtomicBool::new(false);
            let registration = tunnels.register(client, server, &shutdown).unwrap();
            assert_eq!(tunnels.len(), 1);
            drop(registration);
            assert_eq!(tunnels.len(), 0, "completed tunnels retain no descriptors");
        }
        // Phase `revoked_lease_rejects_late_tunnel_registration`.
        {
            let tunnels = ActiveTunnels::default();
            let shutdown = AtomicBool::new(true);
            let (client, server) = connected_pair();
            assert!(tunnels.register(client, server, &shutdown).is_err());
            assert_eq!(tunnels.len(), 0);
        }
        // Phase `a_tunnel_carries_bytes_both_ways_and_revocation_closes_it`.
        {
            let service = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = service.local_addr().unwrap().port();
            let proxy = GuestNetworkProxy::start_at(
                "127.0.0.1:0".parse().unwrap(),
                &["localhost".into()],
                false,
            )
            .unwrap();
            let mut client = TcpStream::connect(proxy.binding().endpoint).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            send_connect(&mut client, port, &proxy.binding().token);
            assert!(read_response(&mut client).starts_with("HTTP/1.1 200"));
            let (mut upstream, _) = service.accept().unwrap();
            upstream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();

            client.write_all(b"ping").unwrap();
            let mut buf = [0_u8; 4];
            upstream.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"ping");
            upstream.write_all(b"pong").unwrap();
            client.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"pong");

            proxy.revoke();
            let mut byte = [0_u8; 1];
            match client.read(&mut byte) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                    ) => {}
                result => panic!("active tunnel survived revoke: {result:?}"),
            }
        }
        // Phase `revocation_interrupts_an_active_tunnel_immediately`.
        {
            let proxy = GuestNetworkProxy::start_at(
                "127.0.0.1:0".parse().unwrap(),
                &["example.test".into()],
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
    }

    /// Send a CONNECT for `localhost:port` carrying the lease token.
    fn send_connect(stream: &mut TcpStream, port: u16, token: &str) {
        stream
            .write_all(
                format!(
                    "CONNECT localhost:{port} HTTP/1.1\r\n\
                     Proxy-Authorization: Bearer {token}\r\n\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
    }

    /// Read a response head from the relay.
    fn read_response(stream: &mut TcpStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        String::from_utf8(head).unwrap()
    }

    #[test]
    fn connections_past_the_limit_wait_for_a_free_slot() {
        let service = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = service.local_addr().unwrap().port();
        let proxy = GuestNetworkProxy::start_limited(
            "127.0.0.1:0".parse().unwrap(),
            &["localhost".into()],
            false,
            1,
        )
        .unwrap();
        let token = proxy.binding().token.clone();

        // The one slot: a tunnel that stays open.
        let mut first = TcpStream::connect(proxy.binding().endpoint).unwrap();
        first
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        send_connect(&mut first, port, &token);
        assert!(read_response(&mut first).starts_with("HTTP/1.1 200"));
        let (held, _) = service.accept().unwrap();

        // The kernel queues a second connection; the relay leaves it there.
        let mut second = TcpStream::connect(proxy.binding().endpoint).unwrap();
        send_connect(&mut second, port, &token);
        second
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut byte = [0_u8; 1];
        match second.read(&mut byte) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            result => panic!("a connection past the limit was served: {result:?}"),
        }

        // Closing the tunnel frees the slot; the waiting CONNECT is served.
        drop(first);
        drop(held);
        second
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        assert!(read_response(&mut second).starts_with("HTTP/1.1 200"));
    }
}
