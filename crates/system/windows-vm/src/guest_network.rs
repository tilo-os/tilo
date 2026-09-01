//! Lease-scoped filtered egress for the one Windows guest.
//!
//! The appliance never receives a NAT interface.  Instead, a Windows-local
//! proxy connects to this host-only listener and authenticates with the
//! unguessable capability lease token below.  The relay checks the original
//! `NetHost` name before resolving it, so a guest application cannot turn a
//! name grant into arbitrary-address access.  Dropping the lease closes the
//! listener; existing and new proxy connections then fail closed.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
}

/// A real, token-authenticated CONNECT relay for one materialized lease.
///
/// It intentionally supports no SOCKS, UDP, BIND, or direct guest route. The
/// guest-side proxy translates normal HTTP proxy traffic to CONNECT, so the
/// host makes exactly one name-based decision before every TCP connection.
pub struct GuestNetworkProxy {
    binding: GuestNetworkBinding,
    shutdown: Arc<AtomicBool>,
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
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_token = binding.token.clone();
        thread::spawn(move || {
            serve(
                listener,
                hosts,
                worker_token,
                require_guest_peer,
                worker_shutdown,
            )
        });
        Ok(Self { binding, shutdown })
    }

    #[must_use]
    pub fn binding(&self) -> &GuestNetworkBinding {
        &self.binding
    }
}

impl Drop for GuestNetworkProxy {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.binding.endpoint);
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

fn serve(
    listener: TcpListener,
    hosts: Vec<String>,
    token: String,
    require_guest_peer: bool,
    shutdown: Arc<AtomicBool>,
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
        let hosts = hosts.clone();
        let token = token.clone();
        let shutdown = Arc::clone(&shutdown);
        thread::spawn(move || {
            let _ = serve_connect(stream, &hosts, &token, &shutdown);
        });
    }
}

fn serve_connect(
    mut client: TcpStream,
    hosts: &[String],
    token: &str,
    shutdown: &AtomicBool,
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
    let policy = EgressPolicy::new(hosts.to_vec());
    if !policy.permits(&host) {
        return write_response(&mut client, "502 Bad Gateway");
    }
    let mut upstream = match connect_upstream(&Destination::Domain(host.clone(), port), &policy) {
        Ok(stream) => stream,
        Err(_) => return write_response(&mut client, "502 Bad Gateway"),
    };
    if shutdown.load(Ordering::Acquire) {
        return write_response(&mut client, "502 Bad Gateway");
    }
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
    if parts.next().is_none() {
        return None;
    }
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
}
