//! Per-host egress filtering for a sandboxed app.
//!
//! ## The problem
//!
//! `bwrap` has no per-host vocabulary. `--unshare-net` gives an app an
//! empty network namespace; `--share-net` gives it the host's. There is
//! no middle setting. So a capability naming `mail.example.com` and a
//! capability naming `*` produced the same sandbox: the named host
//! reached no mechanism at all.
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
//! Two kernel properties make that exclusive rather than advisory, and
//! `tests/netgate_egress.rs` asserts both against a real sandbox: an
//! unshared netns has no route (so `connect(2)` to anywhere fails
//! `ENETUNREACH` before a packet exists, and the sandbox's `127.0.0.1`
//! is its own loopback, not the host's), and a bind-mounted pathname
//! `AF_UNIX` socket crosses the boundary anyway because it is addressed
//! through the filesystem.
//!
//! **An app that ignores the gate is not unfiltered, it is offline.**
//! That is the property that distinguishes this from a proxy an app can
//! decline to use.
//!
//! ## Why the alternatives lose
//!
//! - **nftables in a per-app netns.** Needs connectivity into the netns
//!   first: a `veth` pair needs `CAP_NET_ADMIN` in the *host* namespace,
//!   which the launcher does not have and should not get. `pasta` /
//!   `slirp4netns` avoid the privilege but add a userspace TCP/IP stack
//!   as a hard dependency. And the fatal part is not privilege, it is
//!   vocabulary: a rule set is written against addresses, so it cannot
//!   express "mail.example.com" — see the drift discussion below.
//! - **Landlock `AccessNet`.** ABI 4 scopes TCP `bind`/`connect` by
//!   **port number**. There is no address vocabulary in it at all. It
//!   is a good second layer and [`crate::landlock_shim`] uses it to pin
//!   the app to the relay port, but it can never be the host filter.
//! - **seccomp on `connect(2)`.** A classic filter cannot dereference
//!   the `sockaddr` pointer, so it cannot see the destination.
//!   `SECCOMP_USER_NOTIF` can, at the cost of a synchronous supervisor
//!   on every syscall plus TOCTOU handling — and it still sees only an
//!   address, never a name.
//! - **`LD_PRELOAD` interposition.** Bypassed by a static binary or a
//!   direct syscall. Not a boundary.
//!
//! ## Hostnames are not addresses
//!
//! This is the part that decides whether a filter is real, so it is
//! worth being explicit about all three failure modes:
//!
//! - **Rotating CDN.** The gate never pins an address. It matches the
//!   *name* carried in the SOCKS5 request against the allowlist and
//!   only then resolves, per connection. A host that answers from a
//!   different address on every request works normally, and an
//!   allowlist entry never goes stale.
//! - **DNS changing mid-session.** Same answer: there is no cached
//!   address to go stale. Resolution happens at connect time, after the
//!   policy decision, so a record that changes between two connections
//!   is simply two resolutions.
//! - **The app resolving the name itself.** It cannot. The sandbox has
//!   no route to any resolver (see below), so a client that resolves
//!   before connecting fails at resolution rather than escaping the
//!   filter. This is why the previous two answers hold: the app never
//!   supplies an address for a name-scoped grant, so there is no path
//!   where we match a name and connect to something else.
//!
//! A request that names an **address literal** is matched against the
//! allowlist as that literal. It matches `*`, or an entry that is the
//! same literal; it never matches a hostname entry, because "this
//! address is currently that name" is not something the gate can
//! verify, and accepting it would make every name-scoped grant
//! bypassable by resolving first. Fail-closed, deliberately.
//!
//! ## DNS is egress
//!
//! An app with no network capability reaches no resolver: the namespace
//! is empty and `/etc/resolv.conf` is not bound.
//!
//! An app with an allowlist also reaches no resolver. It does not need
//! one — it hands the gate a name and the gate resolves in the host
//! namespace, on its behalf, *after* the name has passed the policy.
//! So resolution is not a side channel: an app cannot make the gate
//! resolve a name it was not allowed to reach, and it cannot exfiltrate
//! through DNS query contents because it never emits a query.
//!
//! An app granted `*` keeps `--share-net` and resolves for itself, as
//! before. There is nothing to filter, so there is nothing to broker.
//!
//! ## What this does not constrain
//!
//! **Ports.** A `Scope::NetHost` names a host and carries no port, so a
//! grant for `mail.example.com` permits any port on that host. Adding
//! port syntax to the capability vocabulary is a `collab-types` change,
//! not a launcher one.
//!
//! **Content.** The gate is a TCP relay, not a TLS terminator. It sees
//! the requested name and the byte stream, and deliberately does not
//! inspect or intercept the latter.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, TcpStream, ToSocketAddrs};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use collab_types::caps::host_covers;

/// How long a client gets to complete the SOCKS5 handshake, and how
/// long the gate waits on an upstream `connect`. A stuck peer must not
/// pin a thread forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The loopback port the in-sandbox relay listens on. Fixed rather than
/// negotiated: it appears in the app's `ALL_PROXY`, in the relay's argv
/// and in the Landlock rule that pins the app to it, and a fixed value
/// keeps those three from disagreeing. It is a port inside a namespace
/// that contains exactly one listener, so there is nothing to collide
/// with.
pub const RELAY_PORT: u16 = 9050;

/// Where the gate socket is bind-mounted inside the sandbox.
///
/// Short on purpose. `sun_path` is 108 bytes, so a socket that lives at
/// a long path cannot be connected to at all — a failure that surfaces
/// as a confusing `ENAMETOOLONG` at `connect` time rather than at bind
/// time.
pub const SANDBOX_GATE_PATH: &str = "/run/qos/net/gate.sock";

/// Host-side path the gate listens on: `/run/user/<uid>/qos/net/<pid>.sock`.
///
/// Mirrors [`crate::dbus_proxy::proxy_socket_path`] — one per launcher
/// process, so concurrent launches of different apps never share a
/// policy.
pub fn gate_socket_path(uid: u32, launcher_pid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/qos/net/{launcher_pid}.sock"))
}

/// What outbound access an app ends up with, after the packager's
/// manifest and the user's grants have both been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressDecision {
    /// No network at all. The namespace stays empty and no gate runs.
    Blocked,
    /// Unrestricted outbound access — `--share-net`, no gate. There is
    /// no allowlist to enforce, so brokering would add a hop and buy
    /// nothing.
    Unrestricted,
    /// A specific set of hosts. The namespace stays empty and a gate
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
    /// decision.
    ///
    /// - `net_mode` / `manifest_hosts` come from `[sandbox.net]`.
    /// - `granted_hosts` are the hosts the user's `Scope::NetHost`
    ///   capabilities named, already filtered through
    ///   `decide_capability` by [`crate::capabilities`].
    ///
    /// Precedence, and why:
    ///
    /// | manifest | user grants | result |
    /// |---|---|---|
    /// | `off` | none | blocked |
    /// | `allowlist = [h…]` | none | filtered to `h…` |
    /// | `host` | none | unrestricted |
    /// | any | `["*"]` | unrestricted |
    /// | any | `[h…]` | filtered to `h…`, minus anything the manifest's own allowlist does not cover |
    ///
    /// **The user's decision replaces the manifest's, it does not union
    /// with it.** A union would make a narrowing grant useless: an app
    /// whose manifest says `host` would stay unrestricted no matter how
    /// specific the user was, which is the entire bug this exists to
    /// fix.
    ///
    /// **A grant is still capped by an explicit manifest allowlist.** A
    /// packager who enumerated endpoints declared a surface; a runtime
    /// grant naming something outside it is dropped with a warning
    /// rather than silently widening what was audited at install time.
    /// A manifest of `off` or `host` enumerates nothing, so it caps
    /// nothing.
    pub fn resolve(net_mode: &str, manifest_hosts: &[String], granted_hosts: &[String]) -> Self {
        let manifest_is_allowlist = net_mode == "allowlist";

        if granted_hosts.is_empty() {
            return match net_mode {
                // `dns-only` grants no host egress, so it grants none
                // here. It is listed explicitly rather than falling
                // through to the catch-all, where "not off" would have
                // read as "unrestricted" — the same mistake that made
                // an allowlist mean unrestricted. Under this design an
                // app has no resolver of its own in any case, so the
                // mode has nothing left to express.
                "off" | "dns-only" => EgressDecision::Blocked,
                "allowlist" => EgressPolicy::new(manifest_hosts.to_vec()).into_decision(),
                // Any other mode is the historical "share the host
                // network" spelling.
                _ => EgressDecision::Unrestricted,
            };
        }

        let mut kept: Vec<String> = Vec::new();
        for host in granted_hosts {
            if manifest_is_allowlist && !manifest_hosts.iter().any(|c| host_covers(c, host)) {
                tracing::warn!(
                    host = %host,
                    "dropping network grant: the package manifest declares an endpoint \
                     allowlist that does not cover this host, and a runtime grant does not \
                     widen what was declared at install time"
                );
                continue;
            }
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

    /// Does any pattern permit `host`?
    ///
    /// The matcher is `collab_types::caps::host_covers` — the same one
    /// `decide_capability` uses when the compositor evaluates the very
    /// same grant. A launcher-local matcher would be a second
    /// interpretation of a user's allowlist, and the two would drift.
    pub fn permits(&self, host: &str) -> bool {
        self.allow.iter().any(|p| host_covers(p, host))
    }

    /// An allowlist containing `*` is not an allowlist.
    pub fn is_unrestricted(&self) -> bool {
        self.allow.iter().any(|p| p == "*")
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

/// Read the client greeting and answer it. No authentication: the
/// channel is a per-launch socket reachable only from this app's
/// sandbox, so the peer's identity is established by the bind-mount,
/// not by a credential it could quote.
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
pub fn socks5_request<S: Read + Write>(stream: &mut S) -> io::Result<Destination> {
    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    let (ver, cmd, atyp) = (head[0], head[1], head[3]);
    if ver != 0x05 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad SOCKS version"));
    }
    if cmd != 0x01 {
        // BIND and UDP ASSOCIATE would each need their own policy
        // story (an inbound listener, a connectionless path with no
        // name in it). Neither is expressible in a `NetHost` grant, so
        // neither is offered.
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

/// Write a reply header. The bound-address fields are zeroed: a CONNECT
/// client has no use for them and every client we care about ignores
/// them.
pub fn write_reply<S: Write>(stream: &mut S, code: u8) -> io::Result<()> {
    stream.write_all(&[0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
}

fn serve_connection(
    mut client: UnixStream,
    policy: &EgressPolicy,
    stats: &GateStats,
) -> io::Result<()> {
    client.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    client.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;

    socks5_greet(&mut client)?;
    let dest = socks5_request(&mut client)?;
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
        write_reply(&mut client, reply::NOT_ALLOWED)?;
        return Ok(());
    }

    // Resolution happens here, after the policy decision and per
    // connection — never cached, never pinned. See the module docs on
    // name-to-address drift.
    let upstream = match connect_upstream(&dest) {
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

fn connect_upstream(dest: &Destination) -> io::Result<TcpStream> {
    let addrs: Vec<std::net::SocketAddr> = match dest {
        Destination::Domain(host, port) => (host.as_str(), *port).to_socket_addrs()?.collect(),
        Destination::Addr(ip, port) => vec![std::net::SocketAddr::new(*ip, *port)],
    };
    let mut last = io::Error::new(io::ErrorKind::NotFound, "name resolved to no addresses");
    for addr in addrs {
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
/// **This code is untrusted and does not need to be trusted.** It runs
/// inside the sandbox, in the same namespaces as the app, so the app
/// could kill it, replace it, or skip it and speak to the gate socket
/// directly. None of that gains anything: the bytes it carries are
/// opaque to it, every policy decision is made on the host side of the
/// unix socket, and an app that kills it has only disconnected itself.
///
/// It exists so that an unmodified HTTP client honouring `ALL_PROXY`
/// works, since a `socks5h://` URL needs a TCP endpoint and the gate is
/// a unix socket.
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
        hosts.iter().map(|h| h.to_string()).collect()
    }

    #[test]
    fn a_manifest_allowlist_is_enforced_instead_of_being_dropped() {
        // Maps declares five endpoints. Before the gate this parsed to
        // `mode != "off"` and became unrestricted access.
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
        // The canonical manifest vocabulary has a `dns-only` mode. A
        // catch-all reading "not off" as "share the host network" is
        // exactly how an allowlist came to mean unrestricted, so the
        // mode is answered explicitly.
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
        // The bypass this closes: resolve the allowed name yourself,
        // then ask the gate for the address. If a literal matched a
        // name entry, every name-scoped grant would be an any-host
        // grant for anyone who controls a DNS answer.
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
        // `.example.com` is `host_covers`' suffix form — the same
        // spelling the compositor evaluates. Asserted here so a change
        // to the shared matcher shows up as a launcher test failure.
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
}
