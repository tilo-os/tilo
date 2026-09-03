//! Per-host egress filtering for a sandboxed app.
//!
//! ## The problem
//!
//! `bwrap` has no per-host vocabulary: `--unshare-net` gives an empty
//! network namespace, `--share-net` the host's, no middle setting. A
//! capability naming `mail.example.com` and one naming `*` produced the
//! same sandbox — the named host reached no mechanism at all.
//!
//! ## The mechanism
//!
//! Leave the namespace empty and put the only way out through a socket
//! whose far end we hold:
//!
//! ```text
//!  ┌ sandbox: empty netns ─────────────┐
//!  │  app ──TCP──▶ 127.0.0.1:<port>    │   in-sandbox relay, untrusted
//!  │                    │              │
//!  └────────────────────┼──────────────┘
//!                       │ AF_UNIX (the one bind-mount)
//!                       ▼
//!                    netgate                 ◀── host netns, holds the policy
//!                       │ SOCKS5: check the NAME, then resolve, then connect
//!                       ▼
//!                   the network
//! ```
//!
//! Two kernel properties make that exclusive rather than advisory
//! (`tests/netgate_egress.rs` asserts both): an unshared netns has no
//! route (`connect(2)` fails `ENETUNREACH` before a packet exists, and
//! the sandbox's `127.0.0.1` is its own loopback); a bind-mounted
//! pathname `AF_UNIX` socket crosses the boundary anyway, addressed
//! through the filesystem.
//!
//! **An app that ignores the gate is not unfiltered, it is offline** —
//! unlike a proxy an app can decline to use.
//!
//! ## Why the alternatives lose
//!
//! - **nftables in a per-app netns** needs `CAP_NET_ADMIN` in the host
//!   namespace (or a userspace TCP/IP stack), and a rule set is written
//!   against addresses, so it can't express `mail.example.com`.
//! - **Landlock `AccessNet`** scopes by port only, no address vocabulary
//!   — a second layer ([`crate::landlock_shim`] pins the relay port),
//!   never the host filter.
//! - **seccomp on `connect(2)`** can't see a name even with
//!   `SECCOMP_USER_NOTIF`, only an address, at the cost of a
//!   synchronous supervisor.
//! - **`LD_PRELOAD` interposition** is bypassed by a static binary or a
//!   direct syscall.
//!
//! ## Hostnames are not addresses
//!
//! The gate never pins an address — it matches the *name* in the
//! request against the allowlist, then resolves per connection, so a
//! rotating CDN or mid-session DNS change doesn't matter (nothing is
//! cached). The app can't resolve for itself either: the sandbox has no
//! route to any resolver, so it never supplies an address for a
//! name-scoped grant.
//!
//! An **address literal** is matched against the allowlist as that
//! literal — never a hostname entry, since "this address is currently
//! that name" isn't verifiable and accepting it would make every
//! name-scoped grant bypassable by resolving first. Fail-closed,
//! deliberately.
//!
//! ## DNS is egress
//!
//! An app with no network capability reaches no resolver — the
//! namespace is empty, even if `/etc/resolv.conf` is bound; there's no
//! route to any address it names. An app with an allowlist also
//! reaches no resolver: it hands the gate a name, which resolves in the
//! host namespace *after* the name passes policy, so resolution isn't
//! a side channel. An app **can** open port 53 on an already-granted
//! host and speak DNS-over-TCP — not an escape, since any answer is
//! still only usable through the gate. An app granted `*` keeps
//! `--share-net` and resolves for itself, as before.
//!
//! ## Device-local is network
//!
//! qOS runs its own services (collaboration broker, registry, …) on the
//! network like anything else, so device-local addresses get the same
//! policy: no network capability means an empty namespace (the
//! sandbox's `127.0.0.1` is its own); a host allowlist means the gate's
//! host-namespace resolution could reach `127.0.0.1` or the machine's
//! LAN address, so [`is_device_local`] drops the destination *after*
//! resolution unless the allowlist names that host **exactly** (see
//! [`EgressPolicy::permits_device_local`]); `*` means device-local is
//! reachable, since it becomes `--share-net` in production. IPv4-mapped
//! IPv6 forms are folded to v4, so `::ffff:127.0.0.1` is loopback too.
//!
//! ## A refusal looks like an absence of network
//!
//! A denied destination answers SOCKS5 `HOST_UNREACHABLE` and HTTP
//! `502` — not `NOT_ALLOWED`/`403` (the wire spellings of `EACCES`) —
//! since "no route" is a state every client already handles by backing
//! off, while "forbidden" sends the user to fix a permission they can't
//! reach from inside the app. A refusal is instant; a genuinely
//! unroutable host may take until [`CONNECT_TIMEOUT`].
//!
//! ## What this does not constrain
//!
//! **Ports** — `Scope::NetHost` names a host with no port, so a grant
//! permits any port on it. **Content** — the gate is a TCP relay, not a
//! TLS terminator: it sees the name and the byte stream, and doesn't
//! inspect it.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, TcpStream, ToSocketAddrs};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use collab_types::caps::host_covers;

/// How long a client gets to complete the SOCKS5 handshake, and how
/// long the gate waits on an upstream `connect`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The loopback port the in-sandbox relay listens on. Fixed rather
/// than negotiated, so `ALL_PROXY`, the relay's argv and the Landlock
/// rule never disagree.
pub const RELAY_PORT: u16 = 9050;

/// Where the gate socket is bind-mounted inside the sandbox. Short on
/// purpose: `sun_path` is 108 bytes, so a long path fails
/// `ENAMETOOLONG` at connect.
pub const SANDBOX_GATE_PATH: &str = "/run/qos/net/gate.sock";

/// Host-side path the gate listens on. Mirrors
/// [`crate::dbus_proxy::proxy_socket_path`] — one per launcher process.
pub fn gate_socket_path(uid: u32, launcher_pid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/qos/net/{launcher_pid}.sock"))
}

/// What outbound access an app ends up with, after the packager's
/// manifest and the user's grants have both been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressDecision {
    /// No network at all. The namespace stays empty and no gate runs.
    Blocked,
    /// Unrestricted outbound access — `--share-net`, no gate.
    Unrestricted,
    /// A specific set of hosts: the namespace stays empty and a gate
    /// carries the traffic.
    Filtered(EgressPolicy),
}

impl EgressDecision {
    /// Should the sandbox get `--share-net`?
    pub fn shares_host_network(&self) -> bool {
        matches!(self, EgressDecision::Unrestricted)
    }

    /// The policy to run a gate with, if any.
    pub fn policy(&self) -> Option<&EgressPolicy> {
        match self {
            EgressDecision::Filtered(p) => Some(p),
            _ => None,
        }
    }

    /// Resolve the packager's declaration and the user's grants into one
    /// decision. `net_mode`/`manifest_hosts` come from `[sandbox.net]`;
    /// `granted_hosts` are the `Scope::NetHost` capabilities the user
    /// granted, already filtered through `decide_capability`.
    ///
    /// Precedence, and why:
    ///
    /// | manifest | user grants | result |
    /// |---|---|---|
    /// | `off` | none | blocked |
    /// | `allowlist = [h…]` | none | filtered to `h…` |
    /// | `host` | none | unrestricted |
    /// | `off` / `host` | `["*"]` | unrestricted |
    /// | `off` / `host` | `[h…]` | filtered to `h…` |
    /// | `allowlist = [c…]` | any grant | filtered to the `c…` that grant permits |
    ///
    /// **The user's decision replaces the manifest's**, it does not union
    /// with it — a union would leave `host` unrestricted no matter how
    /// specific the user was. **A grant is still capped by an explicit
    /// manifest allowlist**: an out-of-surface grant is dropped with a
    /// warning, not silently widened.
    pub fn resolve(net_mode: &str, manifest_hosts: &[String], granted_hosts: &[String]) -> Self {
        let manifest_is_allowlist = net_mode == "allowlist";

        if granted_hosts.is_empty() {
            return match net_mode {
                // `dns-only` grants no host egress, listed explicitly rather than
                // falling into "not off means unrestricted".
                "off" | "dns-only" => EgressDecision::Blocked,
                "allowlist" => EgressPolicy::new(manifest_hosts.to_vec()).into_decision(),
                // Any other mode is the historical "share the host
                // network" spelling.
                _ => EgressDecision::Unrestricted,
            };
        }

        // An explicit manifest allowlist is exhaustive, so the effective set
        // is the declared entries the grant permits — not the grant filtered
        // against the list, which lets a `*` grant narrow instead of matching
        // nothing.
        if manifest_is_allowlist {
            let kept: Vec<String> = manifest_hosts
                .iter()
                .filter(|c| granted_hosts.iter().any(|g| host_covers(g, c)))
                .cloned()
                .collect();
            for declared in manifest_hosts {
                if !kept.contains(declared) {
                    tracing::info!(
                        host = %declared,
                        "endpoint declared by the package is not covered by the user's network \
                         grant; the app cannot reach it"
                    );
                }
            }
            return EgressPolicy::new(kept).into_decision();
        }

        // No enumeration to cap against: `off` and `host` declare
        // nothing specific, so the grant stands as written.
        let mut kept: Vec<String> = Vec::new();
        for host in granted_hosts {
            if !kept.iter().any(|k| k == host) {
                kept.push(host.clone());
            }
        }
        EgressPolicy::new(kept).into_decision()
    }
}

/// An allowlist of host patterns, and the decision function over it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    allow: Vec<String>,
}

impl EgressPolicy {
    pub fn new(allow: Vec<String>) -> Self {
        Self { allow }
    }

    /// The patterns, for logging and tests.
    pub fn hosts(&self) -> &[String] {
        &self.allow
    }

    /// Does any pattern permit `host`? Uses
    /// `collab_types::caps::host_covers`, the same matcher
    /// `decide_capability` uses.
    pub fn permits(&self, host: &str) -> bool {
        self.allow.iter().any(|p| host_covers(p, host))
    }

    /// An allowlist containing `*` is not an allowlist.
    pub fn is_unrestricted(&self) -> bool {
        self.allow.iter().any(|p| p == "*")
    }

    /// Does the allowlist name `host` **exactly**, not merely cover it?
    /// The narrower question device-local egress turns on — `*` and
    /// `.example.com` cover a host without pointing at one machine; only a
    /// literal entry does.
    pub fn names_exactly(&self, host: &str) -> bool {
        self.allow
            .iter()
            .any(|p| p != "*" && !p.starts_with('.') && p.eq_ignore_ascii_case(host))
    }

    /// May a connection to `host` land on an address of *this device*?
    /// Only when the user pointed at that host specifically, or the
    /// allowlist is `*` (included since in production a `*` grant becomes
    /// `--share-net`, reaching the loopback like any other process).
    pub fn permits_device_local(&self, host: &str) -> bool {
        self.is_unrestricted() || self.names_exactly(host)
    }

    fn into_decision(self) -> EgressDecision {
        if self.allow.is_empty() {
            EgressDecision::Blocked
        } else if self.is_unrestricted() {
            EgressDecision::Unrestricted
        } else {
            EgressDecision::Filtered(self)
        }
    }
}

/// Counts of what the gate decided, so a test can assert on the
/// decision the production path actually took rather than on argv.
#[derive(Debug, Default)]
pub struct GateStats {
    pub allowed: AtomicU64,
    pub denied: AtomicU64,
    pub failed: AtomicU64,
}

impl GateStats {
    pub fn allowed(&self) -> u64 {
        self.allowed.load(Ordering::Relaxed)
    }
    pub fn denied(&self) -> u64 {
        self.denied.load(Ordering::Relaxed)
    }
    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }
}

/// Errors from [`NetGate::start`].
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("creating the gate socket directory {path}: {source}")]
    Mkdir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "binding the egress gate socket at {path}: {source}. The app was granted access to \
         specific hosts, which can only be enforced by brokering its traffic through this \
         socket; without it the sandbox would have to be given unrestricted network access \
         instead, which is not what was granted."
    )]
    Bind {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A running gate. Drop stops it and unlinks the socket.
#[derive(Debug)]
pub struct NetGate {
    socket_path: PathBuf,
    stats: Arc<GateStats>,
    shutdown: Arc<AtomicBool>,
}

impl NetGate {
    /// Bind `socket_path` and serve SOCKS5 against `policy` until
    /// dropped.
    ///
    /// # Errors
    ///
    /// Returns [`GateError::Mkdir`] if the socket's parent directory can't
    /// be created, or [`GateError::Bind`] if the socket itself can't be
    /// bound.
    pub fn start(socket_path: &Path, policy: EgressPolicy) -> Result<NetGate, GateError> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| GateError::Mkdir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // A socket left behind by a crashed launcher would make bind()
        // fail with EADDRINUSE.
        let _ = std::fs::remove_file(socket_path);

        let listener = UnixListener::bind(socket_path).map_err(|source| GateError::Bind {
            path: socket_path.to_path_buf(),
            source,
        })?;

        let stats = Arc::new(GateStats::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let gate = NetGate {
            socket_path: socket_path.to_path_buf(),
            stats: Arc::clone(&stats),
            shutdown: Arc::clone(&shutdown),
        };

        tracing::info!(
            socket = %socket_path.display(),
            hosts = ?policy.hosts(),
            "egress gate listening; the sandbox netns stays empty and this socket is its \
             only route out"
        );

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let policy = policy.clone();
                let stats = Arc::clone(&stats);
                std::thread::spawn(move || {
                    if let Err(e) = serve_connection(stream, &policy, &stats) {
                        tracing::debug!(error = %e, "egress gate connection ended");
                    }
                });
            }
        });

        Ok(gate)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn stats(&self) -> &Arc<GateStats> {
        &self.stats
    }
}

impl Drop for NetGate {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Unblock the accept loop with one throwaway connection.
        let _ = UnixStream::connect(&self.socket_path);
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Where a SOCKS5 client asked to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// `ATYP=0x03`. The name the app asked for — this is what the
    /// allowlist is matched against.
    Domain(String, u16),
    /// `ATYP=0x01` / `0x04`. An address literal.
    Addr(IpAddr, u16),
}

impl Destination {
    /// The string the policy is evaluated against.
    pub fn host_key(&self) -> String {
        match self {
            Destination::Domain(h, _) => h.clone(),
            Destination::Addr(ip, _) => ip.to_string(),
        }
    }

    pub fn port(&self) -> u16 {
        match self {
            Destination::Domain(_, p) | Destination::Addr(_, p) => *p,
        }
    }
}

/// SOCKS5 reply codes we emit.
pub mod reply {
    pub const SUCCESS: u8 = 0x00;
    pub const GENERAL_FAILURE: u8 = 0x01;
    /// "connection not allowed by ruleset" — the allowlist said no.
    pub const NOT_ALLOWED: u8 = 0x02;
    pub const HOST_UNREACHABLE: u8 = 0x04;
    pub const COMMAND_NOT_SUPPORTED: u8 = 0x07;
    pub const ADDRESS_TYPE_NOT_SUPPORTED: u8 = 0x08;
}

/// Read the client greeting and answer it. No authentication: peer
/// identity is established by the bind-mount, not a credential.
///
/// # Errors
///
/// Returns an error if `stream` fails to read/write, or if the version byte
/// is not `0x05` (not SOCKS5).
pub fn socks5_greet<S: Read + Write>(stream: &mut S) -> io::Result<()> {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head)?;
    if head[0] != 0x05 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("not SOCKS5: version byte {:#04x}", head[0]),
        ));
    }
    let mut methods = vec![0u8; head[1] as usize];
    stream.read_exact(&mut methods)?;
    stream.write_all(&[0x05, 0x00])?;
    Ok(())
}

/// Read a CONNECT request. Returns the destination, or writes the
/// appropriate refusal and returns an error.
///
/// # Errors
///
/// Returns an error if `stream` fails to read/write, if the version byte is
/// not `0x05`, if the command is not CONNECT, if the address type is
/// unrecognised, or if a domain name is not valid UTF-8.
pub fn socks5_request<S: Read + Write>(stream: &mut S) -> io::Result<Destination> {
    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    let (ver, cmd, atyp) = (head[0], head[1], head[3]);
    if ver != 0x05 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad SOCKS version",
        ));
    }
    if cmd != 0x01 {
        // BIND and UDP ASSOCIATE would each need their own policy story, and
        // neither is expressible in a `NetHost` grant, so neither is offered.
        write_reply(stream, reply::COMMAND_NOT_SUPPORTED)?;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("SOCKS5 command {cmd:#04x} is not CONNECT"),
        ));
    }

    let dest = match atyp {
        0x01 => {
            let mut raw = [0u8; 4];
            stream.read_exact(&mut raw)?;
            Destination::Addr(IpAddr::from(raw), read_port(stream)?)
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            let mut name = vec![0u8; len[0] as usize];
            stream.read_exact(&mut name)?;
            let name = String::from_utf8(name)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "domain is not UTF-8"))?;
            Destination::Domain(name, read_port(stream)?)
        }
        0x04 => {
            let mut raw = [0u8; 16];
            stream.read_exact(&mut raw)?;
            Destination::Addr(IpAddr::from(raw), read_port(stream)?)
        }
        other => {
            write_reply(stream, reply::ADDRESS_TYPE_NOT_SUPPORTED)?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown SOCKS5 address type {other:#04x}"),
            ));
        }
    };
    Ok(dest)
}

fn read_port<S: Read>(stream: &mut S) -> io::Result<u16> {
    let mut raw = [0u8; 2];
    stream.read_exact(&mut raw)?;
    Ok(u16::from_be_bytes(raw))
}

/// Write a reply header. The bound-address fields are zeroed — a
/// CONNECT client has no use for them.
///
/// # Errors
///
/// Returns an error if `stream` fails to write.
pub fn write_reply<S: Write>(stream: &mut S, code: u8) -> io::Result<()> {
    stream.write_all(&[0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
}

/// Which proxy protocol a client is speaking. Both served on one
/// socket, chosen by the first byte — SOCKS5 is a non-default
/// `reqwest` feature, so an app built without it silently ignores
/// `ALL_PROXY`, while HTTP `CONNECT` works everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Socks5,
    HttpConnect,
}

/// SOCKS5's first byte is its version, `0x05`. No HTTP method begins
/// with a control character, so one byte separates them unambiguously.
pub fn detect_protocol(first_byte: u8) -> Protocol {
    if first_byte == 0x05 {
        Protocol::Socks5
    } else {
        Protocol::HttpConnect
    }
}

fn serve_connection(
    mut client: UnixStream,
    policy: &EgressPolicy,
    stats: &GateStats,
) -> io::Result<()> {
    client.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    client.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;

    let mut first = [0u8; 1];
    client.read_exact(&mut first)?;
    match detect_protocol(first[0]) {
        Protocol::Socks5 => serve_socks5(client, policy, stats),
        Protocol::HttpConnect => serve_http(client, first[0], policy, stats),
    }
}

fn serve_socks5(
    mut client: UnixStream,
    policy: &EgressPolicy,
    stats: &GateStats,
) -> io::Result<()> {
    // The version byte is already consumed by protocol detection; hand
    // the greeting parser a stream that replays it.
    let dest = {
        let mut replayed = Replay::new(vec![0x05], &mut client);
        socks5_greet(&mut replayed)?;
        socks5_request(&mut replayed)?
    };
    let key = dest.host_key();

    // THE decision. Everything before this is parsing and everything
    // after is plumbing.
    if !policy.permits(&key) {
        stats.denied.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            host = %key,
            port = dest.port(),
            allow = ?policy.hosts(),
            "egress DENIED: the app asked for a host it was not granted"
        );
        // `HOST_UNREACHABLE`, not `NOT_ALLOWED`: told there's no route (a state
        // it handles) rather than that it lacks a permission.
        write_reply(&mut client, reply::HOST_UNREACHABLE)?;
        return Ok(());
    }

    // Resolution happens here, after the policy decision and per
    // connection — never cached, never pinned.
    let upstream = match connect_upstream(&dest, policy) {
        Ok(s) => s,
        Err(e) => {
            stats.failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(host = %key, error = %e, "egress allowed but upstream connect failed");
            write_reply(&mut client, reply::HOST_UNREACHABLE)?;
            return Ok(());
        }
    };

    stats.allowed.fetch_add(1, Ordering::Relaxed);
    tracing::info!(host = %key, port = dest.port(), "egress allowed");
    write_reply(&mut client, reply::SUCCESS)?;

    // Past the handshake the stream is a bulk pipe; the handshake
    // timeout would kill a long-lived idle connection.
    client.set_read_timeout(None)?;
    client.set_write_timeout(None)?;
    splice(client, upstream);
    Ok(())
}

/// A reader that replays already-consumed bytes, since protocol
/// detection consumes the first byte the parser then expects to see.
struct Replay<'a, S> {
    pending: std::collections::VecDeque<u8>,
    inner: &'a mut S,
}

impl<'a, S> Replay<'a, S> {
    fn new(pending: Vec<u8>, inner: &'a mut S) -> Self {
        Self {
            pending: pending.into(),
            inner,
        }
    }
}

impl<S: Read> Read for Replay<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut n: usize = 0;
        while n < buf.len() {
            let Some(b) = self.pending.pop_front() else {
                break;
            };
            let Some(slot) = buf.get_mut(n) else { break };
            *slot = b;
            n = n.saturating_add(1);
        }
        if n > 0 {
            return Ok(n);
        }
        self.inner.read(buf)
    }
}

impl<S: Write> Write for Replay<'_, S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// What an HTTP proxy request asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpProxyRequest {
    /// The destination, taken from the `CONNECT` target or the
    /// absolute-form request URI. A **name**, exactly as the client wrote it.
    pub dest: Destination,
    /// True for `CONNECT` (a tunnel), false for an ordinary proxied
    /// request that must be forwarded.
    pub tunnel: bool,
    /// For a forwarded request, the bytes to send upstream: the request line
    /// rewritten to origin-form plus the original headers. Empty for a tunnel.
    pub forward: Vec<u8>,
}

/// Parse an HTTP proxy request: `CONNECT host:port` or an absolute-URI
/// GET. `None` if malformed; default port 443/80.
pub fn parse_http_proxy_head(head: &str) -> Option<HttpProxyRequest> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next().unwrap_or("HTTP/1.1");

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, 443)?;
        return Some(HttpProxyRequest {
            dest: Destination::Domain(host, port),
            tunnel: true,
            forward: Vec::new(),
        });
    }

    // Absolute-form URI; origin servers aren't required to accept it, so
    // rewritten before forwarding.
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("HTTP://"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at_checked(i).unwrap_or((rest, "/")),
        None => (rest, "/"),
    };
    let (host, port) = split_host_port(authority, 80)?;

    let mut forward = format!("{method} {path} {version}\r\n").into_bytes();
    for line in lines {
        forward.extend_from_slice(line.as_bytes());
        forward.extend_from_slice(b"\r\n");
    }
    Some(HttpProxyRequest {
        dest: Destination::Domain(host, port),
        tunnel: false,
        forward,
    })
}

/// `host:port` / `host` / `[v6]:port`, with a default when the port is absent.
fn split_host_port(authority: &str, default_port: u16) -> Option<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        // Bracketed IPv6 literal.
        let (host, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default_port,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => Some((host.to_string(), port.parse().ok()?)),
        _ => Some((authority.to_string(), default_port)),
    }
}

/// Read an HTTP message head (up to and including the blank line).
fn read_http_head<S: Read>(stream: &mut S, seed: u8) -> io::Result<String> {
    let mut buf = vec![seed];
    let mut byte = [0u8; 1];
    loop {
        if buf.len() > 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP proxy request head too large",
            ));
        }
        let n = stream.read(&mut byte)?;
        if n == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(buf).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP proxy request is not UTF-8",
        )
    })
}

fn serve_http(
    mut client: UnixStream,
    first: u8,
    policy: &EgressPolicy,
    stats: &GateStats,
) -> io::Result<()> {
    let head = read_http_head(&mut client, first)?;
    let Some(req) = parse_http_proxy_head(&head) else {
        stats.failed.fetch_add(1, Ordering::Relaxed);
        let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        return Ok(());
    };
    let key = req.dest.host_key();

    if !policy.permits(&key) {
        stats.denied.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            host = %key,
            port = req.dest.port(),
            allow = ?policy.hosts(),
            "egress DENIED: the app asked for a host it was not granted"
        );
        // `502`, not `403`: same reasoning as the SOCKS5 reply code — makes a
        // refused host indistinguishable from an unreachable one.
        let _ = client.write_all(
            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return Ok(());
    }

    let mut upstream = match connect_upstream(&req.dest, policy) {
        Ok(s) => s,
        Err(e) => {
            stats.failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(host = %key, error = %e, "egress allowed but upstream connect failed");
            let _ = client.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return Ok(());
        }
    };

    stats.allowed.fetch_add(1, Ordering::Relaxed);
    tracing::info!(host = %key, port = req.dest.port(), tunnel = req.tunnel, "egress allowed");

    if req.tunnel {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    } else {
        upstream.write_all(&req.forward)?;
    }

    client.set_read_timeout(None)?;
    client.set_write_timeout(None)?;
    splice(client, upstream);
    Ok(())
}

/// Fold an address to its v4 form for the device-local test, so one
/// spelling of loopback can't slip past a test for another.
fn canonical_ip(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => v6.to_ipv4().map_or(addr, IpAddr::V4),
        IpAddr::V4(_) => addr,
    }
}

/// Is `addr` an address of *this device*? Three cases, the third being
/// what a loopback check alone misses: (1) loopback and unspecified;
/// (2) link-local; (3) any address configured on this machine's
/// interfaces — a name resolving to the machine's own LAN address is
/// device-local however public it looks, checked via `bind(2)` failing
/// `EADDRNOTAVAIL`.
///
/// Private ranges as such are *not* device-local — `192.168.1.7` is
/// usually a different machine.
pub fn is_device_local(addr: IpAddr) -> bool {
    // Before folding: `Ipv6Addr::to_ipv4` maps `::1` to `0.0.0.1`, so the
    // v6 loopback has to be recognised as itself first.
    if addr.is_loopback() || addr.is_unspecified() {
        return true;
    }
    let addr = canonical_ip(addr);
    if addr.is_loopback() || addr.is_unspecified() {
        return true;
    }
    let link_local = match addr {
        IpAddr::V4(v4) => v4.is_link_local(),
        // `fe80::/10`, spelled out because `Ipv6Addr::is_unicast_link_local`
        // is not stable.
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    };
    link_local || is_assigned_to_this_host(addr)
}

/// Does this machine hold `addr` on one of its interfaces?
fn is_assigned_to_this_host(addr: IpAddr) -> bool {
    std::net::UdpSocket::bind((addr, 0)).is_ok()
}

/// Resolve and connect, dropping any candidate address that turns out
/// to be this device unless the policy named the host exactly. Runs on
/// the **resolved** address, since a name says nothing about where it
/// points.
/// Connect an already parsed destination after applying the exact same
/// post-DNS device-local protection used by [`NetGate`].  Other confined
/// transports (such as the Windows guest lease relay) must use this rather
/// than resolving a permitted name themselves.
///
/// # Errors
///
/// Returns an error if DNS resolution fails, every resolved address is
/// rejected by policy, or no permitted address can be connected to.
pub fn connect_upstream(dest: &Destination, policy: &EgressPolicy) -> io::Result<TcpStream> {
    let addrs: Vec<std::net::SocketAddr> = match dest {
        Destination::Domain(host, port) => (host.as_str(), *port).to_socket_addrs()?.collect(),
        Destination::Addr(ip, port) => vec![std::net::SocketAddr::new(*ip, *port)],
    };
    let key = dest.host_key();
    let device_local_allowed = policy.permits_device_local(&key);
    let mut last = io::Error::new(io::ErrorKind::NotFound, "name resolved to no addresses");
    for addr in addrs {
        if !device_local_allowed && is_device_local(addr.ip()) {
            tracing::warn!(
                host = %key,
                addr = %addr.ip(),
                allow = ?policy.hosts(),
                "egress DROPPED: the host resolves to this device, and the grant does not name \
                 it exactly. Device-local services are behind the same decision as the rest of \
                 the network."
            );
            last = io::Error::from_raw_os_error(libc::ENETUNREACH);
            continue;
        }
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Pump bytes both ways until either side closes.
fn splice(client: UnixStream, upstream: TcpStream) {
    let (Ok(client_w), Ok(upstream_w)) = (client.try_clone(), upstream.try_clone()) else {
        return;
    };
    let up = std::thread::spawn(move || {
        let mut r = client;
        let mut w = upstream_w;
        let _ = io::copy(&mut r, &mut w);
        let _ = w.shutdown(Shutdown::Write);
    });
    let mut r = upstream;
    let mut w = client_w;
    let _ = io::copy(&mut r, &mut w);
    let _ = w.shutdown(Shutdown::Write);
    let _ = up.join();
}

/// The in-sandbox relay: accept on loopback, carry each connection to
/// the gate socket verbatim.
///
/// **Untrusted and doesn't need to be trusted.** The app could kill or
/// bypass it — the bytes are opaque to it and every policy decision is
/// made on the host side. Exists so an unmodified HTTP client honouring
/// `ALL_PROXY` works, since `socks5h://` needs a TCP endpoint.
///
/// # Errors
///
/// Returns an error if the loopback listener on `port` cannot be bound.
pub fn run_relay(gate_socket: &Path, port: u16) -> io::Result<()> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    let gate = gate_socket.to_path_buf();
    for stream in listener.incoming() {
        let Ok(tcp) = stream else { continue };
        let gate = gate.clone();
        std::thread::spawn(move || {
            let Ok(unix) = UnixStream::connect(&gate) else {
                return;
            };
            relay_pump(tcp, unix);
        });
    }
    Ok(())
}

fn relay_pump(tcp: std::net::TcpStream, unix: UnixStream) {
    let (Ok(tcp_w), Ok(unix_w)) = (tcp.try_clone(), unix.try_clone()) else {
        return;
    };
    let up = std::thread::spawn(move || {
        let mut r = tcp;
        let mut w = unix_w;
        let _ = io::copy(&mut r, &mut w);
        let _ = w.shutdown(Shutdown::Write);
    });
    let mut r = unix;
    let mut w = tcp_w;
    let _ = io::copy(&mut r, &mut w);
    let _ = w.shutdown(Shutdown::Write);
    let _ = up.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(hosts: &[&str]) -> Vec<String> {
        hosts.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn a_manifest_allowlist_is_enforced_instead_of_being_dropped() {
        let maps = v(&[
            "tile.openstreetmap.org",
            "nominatim.openstreetmap.org",
            "overpass-api.de",
            "router.project-osrm.org",
            "api.transitous.org",
        ]);
        let decision = EgressDecision::resolve("allowlist", &maps, &[]);
        let policy = decision.policy().expect("an allowlist must be filtered");
        assert!(policy.permits("tile.openstreetmap.org"));
        assert!(
            !policy.permits("evil.example.com"),
            "a host outside the manifest's allowlist must not be permitted"
        );
        assert!(
            !decision.shares_host_network(),
            "a filtered decision must not share the host network: {decision:?}"
        );
    }

    #[test]
    fn a_host_manifest_with_no_grants_stays_unrestricted() {
        // The ten apps whose endpoint is user data keep working
        // exactly as before when the user has granted nothing.
        assert_eq!(
            EgressDecision::resolve("host", &[], &[]),
            EgressDecision::Unrestricted
        );
    }

    #[test]
    fn no_manifest_network_and_no_grant_is_blocked() {
        assert_eq!(
            EgressDecision::resolve("off", &[], &[]),
            EgressDecision::Blocked
        );
    }

    #[test]
    fn dns_only_does_not_fall_through_to_unrestricted() {
        // The canonical manifest vocabulary has a `dns-only` mode, answered
        // explicitly rather than falling into "not off means unrestricted".
        assert_eq!(
            EgressDecision::resolve("dns-only", &[], &[]),
            EgressDecision::Blocked
        );
    }

    #[test]
    fn a_specific_grant_narrows_a_host_manifest() {
        // The point of the whole exercise: a user who granted one host
        // to an app whose manifest asks for `host` gets one host.
        let decision = EgressDecision::resolve("host", &[], &v(&["mail.example.com"]));
        let policy = decision.policy().expect("a named host must be filtered");
        assert!(policy.permits("mail.example.com"));
        assert!(!policy.permits("github.com"));
        assert!(
            !decision.shares_host_network(),
            "a grant naming one host must not share the whole host network: {decision:?}"
        );
    }

    #[test]
    fn a_star_grant_is_unrestricted() {
        assert_eq!(
            EgressDecision::resolve("host", &[], &v(&["*"])),
            EgressDecision::Unrestricted
        );
        assert_eq!(
            EgressDecision::resolve("off", &[], &v(&["*"])),
            EgressDecision::Unrestricted
        );
    }

    #[test]
    fn a_star_grant_narrows_to_an_explicit_manifest_allowlist() {
        // "Allow everything this app asks for" must mean the declared
        // endpoints, not the whole internet.
        let weather = v(&["api.open-meteo.com", "geocoding-api.open-meteo.com"]);
        let decision = EgressDecision::resolve("allowlist", &weather, &v(&["*"]));
        let policy = decision
            .policy()
            .unwrap_or_else(|| panic!("expected the declared endpoints, got {decision:?}"));
        assert_eq!(policy.hosts(), weather.as_slice());
        assert!(
            !decision.shares_host_network(),
            "a `*` grant must not widen a package that enumerated its endpoints: {decision:?}"
        );
    }

    #[test]
    fn a_suffix_grant_selects_the_declared_endpoints_under_it() {
        let maps = v(&[
            "tile.openstreetmap.org",
            "nominatim.openstreetmap.org",
            "overpass-api.de",
        ]);
        let decision = EgressDecision::resolve("allowlist", &maps, &v(&[".openstreetmap.org"]));
        let policy = decision.policy().expect("filtered");
        assert_eq!(
            policy.hosts(),
            ["tile.openstreetmap.org", "nominatim.openstreetmap.org"]
        );
        assert!(
            !policy.permits("overpass-api.de"),
            "an endpoint outside the granted suffix must not survive: {policy:?}"
        );
    }

    #[test]
    fn a_grant_cannot_exceed_an_explicit_manifest_allowlist() {
        let decision = EgressDecision::resolve(
            "allowlist",
            &v(&["api.open-meteo.com"]),
            &v(&["api.open-meteo.com", "exfil.example.com"]),
        );
        let policy = decision.policy().expect("filtered");
        assert!(policy.permits("api.open-meteo.com"));
        assert!(
            !policy.permits("exfil.example.com"),
            "a runtime grant must not widen the endpoint surface the packager declared: \
             {policy:?}"
        );
    }

    #[test]
    fn a_grant_outside_the_manifest_allowlist_leaving_nothing_blocks() {
        let decision = EgressDecision::resolve(
            "allowlist",
            &v(&["api.open-meteo.com"]),
            &v(&["exfil.example.com"]),
        );
        assert_eq!(decision, EgressDecision::Blocked);
    }

    #[test]
    fn an_address_literal_never_matches_a_hostname_grant() {
        // The bypass this closes: resolving the allowed name yourself then
        // asking the gate for the address would make every name-scoped grant
        // an any-host grant.
        let policy = EgressPolicy::new(v(&["mail.example.com"]));
        assert!(policy.permits("mail.example.com"));
        assert!(
            !policy.permits("93.184.216.34"),
            "an address literal must not satisfy a hostname allowlist entry"
        );
        // An entry that IS a literal still works, for a grant the user
        // actually phrased that way.
        let by_addr = EgressPolicy::new(v(&["93.184.216.34"]));
        assert!(by_addr.permits("93.184.216.34"));
    }

    #[test]
    fn subdomain_grants_use_the_shared_matcher() {
        // `.example.com` is `host_covers`' suffix form, the same spelling the
        // compositor evaluates — asserted so a matcher change shows up here.
        let policy = EgressPolicy::new(v(&[".example.com"]));
        assert!(policy.permits("example.com"));
        assert!(policy.permits("mail.example.com"));
        assert!(!policy.permits("example.com.evil.net"));
        assert!(!policy.permits("notexample.com"));
    }

    #[test]
    fn gate_socket_path_is_short_enough_for_sun_path() {
        // 108 bytes including the NUL. A path over the limit cannot be
        // connected to, and the failure surfaces far from its cause.
        let p = gate_socket_path(1000, 4_194_304);
        assert!(
            p.as_os_str().len() < 100,
            "gate socket path {p:?} is too long for sun_path"
        );
        assert!(SANDBOX_GATE_PATH.len() < 100);
    }

    /// Drive the real handshake parser over an in-memory stream.
    struct Pipe {
        input: io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }
    impl Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.output.flush()
        }
    }

    fn pipe(bytes: Vec<u8>) -> Pipe {
        Pipe {
            input: io::Cursor::new(bytes),
            output: Vec::new(),
        }
    }

    #[test]
    fn parses_a_domain_connect_request() {
        let mut req = vec![0x05, 0x01, 0x00, 0x03, 11];
        req.extend_from_slice(b"example.com");
        req.extend_from_slice(&443u16.to_be_bytes());
        let mut p = pipe(req);
        assert_eq!(
            socks5_request(&mut p).unwrap(),
            Destination::Domain("example.com".to_string(), 443)
        );
    }

    #[test]
    fn parses_an_ipv4_connect_request() {
        let mut req = vec![0x05, 0x01, 0x00, 0x01, 198, 51, 100, 9];
        req.extend_from_slice(&80u16.to_be_bytes());
        let mut p = pipe(req);
        let dest = socks5_request(&mut p).unwrap();
        assert_eq!(dest.host_key(), "198.51.100.9");
        assert_eq!(dest.port(), 80);
    }

    #[test]
    fn a_non_connect_command_is_refused_with_a_reply() {
        // 0x03 is UDP ASSOCIATE.
        let req = vec![0x05, 0x03, 0x00, 0x01, 1, 2, 3, 4, 0, 80];
        let mut p = pipe(req);
        assert!(socks5_request(&mut p).is_err());
        assert_eq!(
            p.output[1],
            reply::COMMAND_NOT_SUPPORTED,
            "a refused command must still get a well-formed SOCKS reply so the client fails \
             fast instead of hanging: {:?}",
            p.output
        );
    }

    #[test]
    fn greeting_rejects_a_non_socks5_peer() {
        let mut p = pipe(vec![0x04, 0x01]);
        assert!(socks5_greet(&mut p).is_err());
    }

    #[test]
    fn the_first_byte_picks_the_protocol() {
        assert_eq!(detect_protocol(0x05), Protocol::Socks5);
        // Every HTTP method starts with an uppercase letter.
        for m in *b"CGPH" {
            assert_eq!(detect_protocol(m), Protocol::HttpConnect);
        }
    }

    #[test]
    fn connect_carries_the_name_the_client_asked_for() {
        // The property that matters: an HTTP CONNECT names a host, so the
        // policy sees a name, same as socks5h, without a client feature flag.
        let req = parse_http_proxy_head(
            "CONNECT api.open-meteo.com:443 HTTP/1.1\r\nHost: api.open-meteo.com:443\r\n\r\n",
        )
        .expect("parses");
        assert_eq!(
            req.dest,
            Destination::Domain("api.open-meteo.com".to_string(), 443)
        );
        assert!(req.tunnel);
    }

    #[test]
    fn connect_without_a_port_defaults_to_443() {
        let req = parse_http_proxy_head("CONNECT example.com HTTP/1.1\r\n\r\n").expect("parses");
        assert_eq!(
            req.dest,
            Destination::Domain("example.com".to_string(), 443)
        );
    }

    #[test]
    fn an_absolute_form_request_is_rewritten_to_origin_form() {
        // A plain-HTTP proxied request arrives with an absolute URI, which
        // origin servers aren't required to accept, so it's rewritten first.
        let req = parse_http_proxy_head(
            "GET http://tile.openstreetmap.org/1/2/3.png HTTP/1.1\r\nUser-Agent: x\r\n\r\n",
        )
        .expect("parses");
        assert_eq!(
            req.dest,
            Destination::Domain("tile.openstreetmap.org".to_string(), 80)
        );
        assert!(!req.tunnel);
        let forward = String::from_utf8(req.forward).unwrap();
        assert!(
            forward.starts_with("GET /1/2/3.png HTTP/1.1\r\n"),
            "request line must be origin-form: {forward:?}"
        );
        assert!(
            forward.contains("User-Agent: x"),
            "headers must survive the rewrite: {forward:?}"
        );
    }

    #[test]
    fn an_ipv6_literal_authority_parses() {
        let req =
            parse_http_proxy_head("CONNECT [2001:db8::1]:8443 HTTP/1.1\r\n\r\n").expect("parses");
        assert_eq!(
            req.dest,
            Destination::Domain("2001:db8::1".to_string(), 8443)
        );
        // And it is still matched as a literal, so it cannot satisfy a
        // hostname grant.
        let policy = EgressPolicy::new(v(&["example.com"]));
        assert!(!policy.permits(&req.dest.host_key()));
    }

    #[test]
    fn a_malformed_head_parses_to_nothing_rather_than_a_default_host() {
        assert!(parse_http_proxy_head("").is_none());
        assert!(parse_http_proxy_head("GARBAGE\r\n\r\n").is_none());
        // An absolute-form URI with a scheme we do not proxy must not
        // silently become a hostname.
        assert!(parse_http_proxy_head("GET ftp://example.com/x HTTP/1.1\r\n\r\n").is_none());
    }

    // Device-local egress.

    #[test]
    fn loopback_and_link_local_are_device_local_in_every_spelling() {
        for addr in [
            "127.0.0.1",
            "127.0.0.53",
            "0.0.0.0",
            "::1",
            "::",
            "169.254.169.254",
            "fe80::1",
            // IPv4-mapped and IPv4-compatible loopback. A test written
            // only against `is_loopback()` misses both.
            "::ffff:127.0.0.1",
            "::127.0.0.1",
        ] {
            assert!(
                is_device_local(addr.parse().unwrap()),
                "{addr} is an address of this device"
            );
        }
    }

    #[test]
    fn a_public_address_is_not_device_local() {
        // Reserved for documentation (RFC 5737 / 3849), so no machine
        // running this test holds them.
        for addr in ["198.51.100.9", "203.0.113.7", "2001:db8::1"] {
            assert!(
                !is_device_local(addr.parse().unwrap()),
                "{addr} is not this device"
            );
        }
    }

    #[test]
    fn this_hosts_own_addresses_are_device_local_even_when_they_look_public() {
        // Every address the host holds routes to its own services; loopback
        // is assertable without knowing the network.
        assert!(is_device_local("127.0.0.1".parse().unwrap()));
        // The mechanism itself: an address the host does not hold cannot
        // be bound.
        assert!(!is_assigned_to_this_host("198.51.100.9".parse().unwrap()));
    }

    #[test]
    fn only_an_exact_grant_or_the_wildcard_reaches_this_device() {
        let suffix = EgressPolicy::new(v(&[".localhost"]));
        assert!(
            suffix.permits("localhost"),
            "the suffix pattern does cover the name — the access decision is unchanged"
        );
        assert!(
            !suffix.permits_device_local("localhost"),
            "but covering a name is not naming it, so it must not reach the device itself"
        );

        let exact = EgressPolicy::new(v(&["localhost"]));
        assert!(exact.permits_device_local("localhost"));

        // A wildcard allowlist becomes `--share-net` in production; the gate
        // must not be stricter than the sandbox it stands in for.
        assert!(EgressPolicy::new(v(&["*"])).permits_device_local("localhost"));

        // An exact grant for one host does not carry device-local access
        // for a different one.
        assert!(!exact.permits_device_local("other.localhost"));
    }

    // The production connect path, against a real listener on the host's
    // loopback — the shape a local service actually has.
    #[test]
    fn a_suffix_grant_cannot_reach_a_loopback_service_an_exact_grant_can() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback service");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = listener.accept() {
                let _ = s.write_all(b"LOCAL-SERVICE");
            }
        });

        let dest = Destination::Domain("localhost".to_string(), port);

        let suffix = EgressPolicy::new(v(&[".localhost"]));
        let err = connect_upstream(&dest, &suffix)
            .expect_err("a suffix grant must not reach this device");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::ENETUNREACH),
            "and the failure must look like an absence of network, not a refusal: {err}"
        );

        let exact = EgressPolicy::new(v(&["localhost"]));
        let mut stream = connect_upstream(&dest, &exact)
            .expect("an exact grant for a device-local host must reach it");
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .expect("read the service reply");
        assert_eq!(
            String::from_utf8_lossy(&buf),
            "LOCAL-SERVICE",
            "the same service, the same request — only the spelling of the grant differed"
        );
    }
}
