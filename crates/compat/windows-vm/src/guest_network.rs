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
//! ([`launcher::egress_proxy::egress_runtime`]), at most
//! [`launcher::egress_proxy::MAX_CONNECTIONS`] at once; past that the listener
//! stops accepting and further guest connections wait in its backlog.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use getrandom::{SysRng, rand_core::UnwrapErr};
use launcher::egress_proxy::{Destination, EgressPolicy, NetHostPattern, connect_upstream_async};
use rand::Rng as _;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, Semaphore};

/// Tilo's immutable host-only gateway. This is not an internet route.
pub const HOST_ONLY_GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 251, 1);
const GUEST_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 251, 2);
const MAX_HEAD_BYTES: usize = 16 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Opaque proxy details carried only over the mutually-authenticated bridge.
/// The token is not a `NetHost` grant; it merely proves that the guest-local
/// proxy is using the already materialized short-lived lease.
///
/// A launch keeps the binding in its keeper's directory while its app lives: the guest
/// holds the endpoint and token, so the desktop that adopts the app after an in-place restart
/// must serve them on the same listener, with the workspace's grants as they stand then.
#[derive(Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuestNetworkBinding {
    pub endpoint: SocketAddr,
    pub token: String,
}

impl GuestNetworkBinding {
    /// Write the binding, readable by this person alone, into the kept launch's directory
    /// `dir`: the note a later desktop serves its network from.
    ///
    /// # Errors
    /// The binding could not be written.
    pub fn persist(&self, dir: &std::path::Path) -> io::Result<()> {
        let path = dir.join(launcher::egress_proxy::APPLIANCE_NETWORK);
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        tilo_files::native::atomic::write_with_mode(&path, &bytes, 0o600)
    }

    /// The binding kept in the kept launch's directory `dir`, when one is.
    ///
    /// # Errors
    /// There is no readable binding there.
    pub fn read(dir: &std::path::Path) -> io::Result<Self> {
        let bytes = std::fs::read(dir.join(launcher::egress_proxy::APPLIANCE_NETWORK))?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    /// Forget the binding in `dir`: the launch ended, and no listener may start from it again.
    pub fn forget(dir: &std::path::Path) {
        let _ = std::fs::remove_file(dir.join(launcher::egress_proxy::APPLIANCE_NETWORK));
    }
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
    #[error("a kept presentation's network token is not one the guest proxy presents")]
    InvalidToken,
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

    pub fn start_at(
        address: SocketAddr,
        hosts: &[String],
        require_guest_peer: bool,
    ) -> Result<Self, GuestNetworkError> {
        Self::start_limited(
            address,
            hosts,
            require_guest_peer,
            launcher::egress_proxy::MAX_CONNECTIONS,
            None,
        )
    }

    /// Serve the lease again on the endpoint and token its guest was given, as the desktop that
    /// adopts a kept presentation after a restart does: the guest keeps using what it holds, and
    /// the listener is back with the grants as they stand now. A token that is not the shape the
    /// guest proxy presents is refused, so a file someone else wrote cannot be served.
    pub fn start_at_with_token(
        address: SocketAddr,
        allowed: &[String],
        require_guest_peer: bool,
        token: String,
    ) -> Result<Self, GuestNetworkError> {
        if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(GuestNetworkError::InvalidToken);
        }
        Self::start_limited(
            address,
            allowed,
            require_guest_peer,
            launcher::egress_proxy::MAX_CONNECTIONS,
            Some(token),
        )
    }

    fn start_limited(
        address: SocketAddr,
        hosts: &[String],
        require_guest_peer: bool,
        max_connections: usize,
        token: Option<String>,
    ) -> Result<Self, GuestNetworkError> {
        let policy = lease_policy(hosts)?;
        let listener = std::net::TcpListener::bind(address).map_err(GuestNetworkError::Bind)?;
        let endpoint = listener.local_addr().map_err(GuestNetworkError::Bind)?;
        listener
            .set_nonblocking(true)
            .map_err(GuestNetworkError::Bind)?;
        let runtime =
            launcher::egress_proxy::egress_runtime().map_err(GuestNetworkError::Runtime)?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener).map_err(GuestNetworkError::Bind)?
        };
        let token = token.unwrap_or_else(|| {
            let mut random = [0_u8; 32];
            UnwrapErr(SysRng).fill_bytes(&mut random);
            let token = hex::encode(random);
            random.fill(0);
            token
        });
        let binding = GuestNetworkBinding {
            endpoint,
            token: token.clone(),
        };
        let lease = Arc::new(Lease {
            policy,
            token,
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
