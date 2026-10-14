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
//! An **address literal** never matches a hostname entry, since "this
//! address is currently that name" isn't verifiable and accepting it
//! would make every name-scoped grant bypassable by resolving first. Nor
//! is an address a grant: the allowlist is an [`EgressPolicy`] read
//! through the one grant grammar (`collab_types::caps::NetHostPattern`,
//! the same one environments' `tilo-egress` reads), which refuses IP
//! literals, so only `*` reaches an address. Fail-closed, deliberately.
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
//! Tilo runs its own services (collaboration broker, registry, …) on the
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
//! That last case is the one place an app's grant reaches further than
//! the same grant does for an environment, whose `*` keeps a namespace of
//! its own (`tilo-egress`). It is kept on purpose: apps whose manifests
//! say `host` take OAuth redirects on a loopback listener the host's
//! browser must reach, and the gate carries TCP only. The grammar and the
//! decision are shared; the backend differs
//! (`docs/architecture/security.md`).
//!
//! ## A refusal looks like an absence of network
//!
//! A denied destination answers SOCKS5 `HOST_UNREACHABLE` and HTTP
//! `502` — not `NOT_ALLOWED`/`403` (the wire spellings of `EACCES`) —
//! since "no route" is a state every client already handles by backing
//! off, while "forbidden" sends the user to fix a permission they can't
//! reach from inside the app. A refusal is instant; a genuinely
//! unroutable host may take until `CONNECT_TIMEOUT`.
//!
//! ## What this does not constrain
//!
//! **Ports** — `Scope::NetHost` names a host with no port, so a grant
//! permits any port on it. **Content** — the gate is a TCP relay, not a
//! TLS terminator: it sees the name and the byte stream, and doesn't
//! inspect it.

use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub use collab_types::caps::{EgressPolicy, NetHostPattern};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;
use tokio::sync::{Semaphore, oneshot};

/// How long a client gets for each read and write of the handshake, and
/// how long the gate waits on an upstream `connect`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How many connections one gate, or one relay, carries at once. Past
/// this it stops accepting: further connections wait in the socket's
/// listen backlog until an open one closes, rather than each costing a
/// task and two descriptors the moment it arrives.
pub const MAX_CONNECTIONS: usize = 256;

/// The loopback port the in-sandbox relay listens on. Fixed rather
/// than negotiated, so `ALL_PROXY`, the relay's argv and the Landlock
/// rule never disagree.
pub const RELAY_PORT: u16 = 9050;

/// Where the gate socket is bind-mounted inside the sandbox. Short on
/// purpose: `sun_path` is 108 bytes, so a long path fails
/// `ENAMETOOLONG` at connect.
pub const SANDBOX_GATE_PATH: &str = "/run/tilo/net/gate.sock";

/// Host-side path the gate listens on. Mirrors
/// [`crate::dbus_proxy::proxy_socket_path`] — one per launcher process.
pub fn gate_socket_path(uid: u32, launcher_pid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/tilo/net/{launcher_pid}.sock"))
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
    /// | any | only non-hosts (`10.0.0.5`) | blocked |
    ///
    /// Grants and manifest entries are both read through the one grant
    /// grammar ([`EgressPolicy::from_grants`]), the same one environments
    /// get: `*.example.com` is the domain, and an IP literal is ignored.
    ///
    /// **The user's decision replaces the manifest's**, it does not union
    /// with it — a union would leave `host` unrestricted no matter how
    /// specific the user was. **A grant is still capped by an explicit
    /// manifest allowlist**: an out-of-surface grant is dropped with a
    /// warning, not silently widened.
    pub fn resolve(net_mode: &str, manifest_hosts: &[String], granted_hosts: &[String]) -> Self {
        let declared = || EgressPolicy::from_grants(manifest_hosts.iter().map(String::as_str));

        if granted_hosts.is_empty() {
            return match net_mode {
                // `dns-only` grants no host egress, listed explicitly rather than
                // falling into "not off means unrestricted".
                "off" | "dns-only" => EgressDecision::Blocked,
                "allowlist" => decision_of(declared()),
                "host" => EgressDecision::Unrestricted,
                // A spelling this launcher does not know is not a policy it
                // can enforce. The only safe reading of "allow-list" or
                // "Off" is no egress — not the host network.
                other => {
                    tracing::warn!(
                        mode = other,
                        "unknown [sandbox.net].mode; the app gets no network"
                    );
                    EgressDecision::Blocked
                }
            };
        }

        // Read through the one grammar: a grant that is not a host (an IP
        // literal, junk) is dropped with a warning, so it narrows. A user
        // whose every grant was refused gets no network, not the manifest's.
        let granted = EgressPolicy::from_grants(granted_hosts.iter().map(String::as_str));

        // An explicit manifest allowlist is exhaustive, so the effective set
        // is the declared entries the grant covers in full — not the grant
        // filtered against the list, which lets a `*` grant narrow instead of
        // matching nothing.
        if net_mode == "allowlist" {
            let EgressPolicy::Hosts(declared) = declared() else {
                // A manifest allowlist of `*` declares nothing to cap with.
                return decision_of(granted);
            };
            let mut kept = Vec::new();
            for pattern in declared {
                if EgressPolicy::hosts(vec![pattern.clone()]).within(&granted) {
                    kept.push(pattern);
                } else {
                    tracing::info!(
                        host = %pattern,
                        "endpoint declared by the package is not covered by the user's network \
                         grant; the app cannot reach it"
                    );
                }
            }
            return decision_of(EgressPolicy::hosts(kept));
        }

        // No enumeration to cap against: `off` and `host` declare
        // nothing specific, so the grant stands as written.
        decision_of(granted)
    }
}

/// What outbound access a policy alone amounts to: nothing, everything
/// (`*`, which for an app is `--share-net`), or a gate.
fn decision_of(policy: EgressPolicy) -> EgressDecision {
    match policy {
        EgressPolicy::Any => EgressDecision::Unrestricted,
        EgressPolicy::Hosts(ref hosts) if hosts.is_empty() => EgressDecision::Blocked,
        filtered @ EgressPolicy::Hosts(_) => EgressDecision::Filtered(filtered),
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
    #[error("starting the runtime that serves the egress gate: {0}")]
    Runtime(#[source] io::Error),
}

/// A running gate. Drop stops it and unlinks the socket.
///
/// Every gate in the process serves on one shared, bounded runtime; each
/// connection is a task on it, and at most [`MAX_CONNECTIONS`] are open
/// per gate.
#[derive(Debug)]
pub struct NetGate {
    socket_path: PathBuf,
    stats: Arc<GateStats>,
    /// Dropping this sender ends the accept loop, which closes the
    /// listener. Connections already open run to completion.
    _stop: oneshot::Sender<()>,
}

impl NetGate {
    /// Bind `socket_path` and serve SOCKS5 against `policy` until
    /// dropped.
    ///
    /// # Errors
    ///
    /// Returns [`GateError::Mkdir`] if the socket's parent directory can't
    /// be created, [`GateError::Bind`] if the socket itself can't be
    /// bound, or [`GateError::Runtime`] if the runtime that serves it
    /// can't start.
    pub fn start(socket_path: &Path, policy: EgressPolicy) -> Result<NetGate, GateError> {
        Self::start_limited(socket_path, policy, MAX_CONNECTIONS)
    }

    fn start_limited(
        socket_path: &Path,
        policy: EgressPolicy,
        max_connections: usize,
    ) -> Result<NetGate, GateError> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| GateError::Mkdir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // A socket left behind by a crashed launcher would make bind()
        // fail with EADDRINUSE.
        let _ = std::fs::remove_file(socket_path);

        let bind_error = |source| GateError::Bind {
            path: socket_path.to_path_buf(),
            source,
        };
        let listener = std::os::unix::net::UnixListener::bind(socket_path).map_err(bind_error)?;
        listener.set_nonblocking(true).map_err(bind_error)?;
        let runtime = egress_runtime().map_err(GateError::Runtime)?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::UnixListener::from_std(listener).map_err(bind_error)?
        };

        tracing::info!(
            socket = %socket_path.display(),
            hosts = ?policy.describe(),
            "egress gate listening; the sandbox netns stays empty and this socket is its \
             only route out"
        );
        let (stats, stop) = serve_gate(runtime, listener, policy, max_connections);
        Ok(NetGate {
            socket_path: socket_path.to_path_buf(),
            stats,
            _stop: stop,
        })
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
        // `_stop` is dropped right after this, which ends the accept loop.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// A gate on a loopback TCP port, for a host with no network namespace to
/// leave empty (macOS): there the sandbox profile admits connections to this
/// one port, and the proxy environment names it. Same protocols, same policy
/// and the same device-local rule as [`NetGate`]; dropping it stops it.
///
/// Any local process can connect to the port. That grants nothing: an
/// unconfined process already has the network, and what the gate reaches is
/// at most what the policy names.
#[derive(Debug)]
pub struct LoopbackGate {
    addr: SocketAddr,
    stats: Arc<GateStats>,
    _stop: oneshot::Sender<()>,
}

impl LoopbackGate {
    /// Bind a free port on `127.0.0.1` and serve `policy` on it until
    /// dropped.
    ///
    /// # Errors
    ///
    /// Returns [`GateError::Bind`] if no port can be bound, or
    /// [`GateError::Runtime`] if the runtime that serves it can't start.
    pub fn start(policy: EgressPolicy) -> Result<LoopbackGate, GateError> {
        let bind_error = |source| GateError::Bind {
            path: PathBuf::from("127.0.0.1:0"),
            source,
        };
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(bind_error)?;
        let addr = listener.local_addr().map_err(bind_error)?;
        listener.set_nonblocking(true).map_err(bind_error)?;
        let runtime = egress_runtime().map_err(GateError::Runtime)?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener).map_err(bind_error)?
        };
        tracing::info!(%addr, hosts = ?policy.describe(), "loopback egress gate listening");
        let (stats, stop) = serve_gate(runtime, listener, policy, MAX_CONNECTIONS);
        Ok(LoopbackGate {
            addr,
            stats,
            _stop: stop,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stats(&self) -> &Arc<GateStats> {
        &self.stats
    }
}

/// Serve every connection `listener` accepts against `policy` on
/// `runtime`, until the returned sender is dropped.
fn serve_gate<L>(
    runtime: &tokio::runtime::Handle,
    listener: L,
    policy: EgressPolicy,
    max_connections: usize,
) -> (Arc<GateStats>, oneshot::Sender<()>)
where
    L: Acceptor,
    L::Conn: AsyncRead + AsyncWrite + Unpin,
{
    let stats = Arc::new(GateStats::default());
    let (stop, stopped) = oneshot::channel();
    let policy = Arc::new(policy);
    let served = Arc::clone(&stats);
    runtime.spawn(accept_bounded(
        listener,
        max_connections,
        stopped,
        move |client| {
            let policy = Arc::clone(&policy);
            let stats = Arc::clone(&served);
            async move {
                if let Err(e) = serve_connection(client, &policy, &stats).await {
                    tracing::debug!(error = %e, "egress gate connection ended");
                }
            }
        },
    ));
    (stats, stop)
}

/// The proxy environment that sends an ordinary HTTP client through a gate
/// listening on `127.0.0.1:port`.
///
/// Both proxy forms are advertised, since picking one breaks apps:
/// `HTTP(S)_PROXY` works with no feature flags (`reqwest`'s SOCKS support is
/// non-default); `ALL_PROXY` is `socks5h://…`, where the `h` sends the
/// **hostname** to the proxy instead of resolving locally, which a confined
/// app cannot do.
pub fn proxy_env(port: u16) -> Vec<(&'static str, String)> {
    let http = format!("http://127.0.0.1:{port}");
    let socks = format!("socks5h://127.0.0.1:{port}");
    ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"]
        .into_iter()
        .map(|var| (var, http.clone()))
        .chain(
            ["ALL_PROXY", "all_proxy"]
                .into_iter()
                .map(|var| (var, socks.clone())),
        )
        .collect()
}

/// The runtime every egress relay in this process serves on — each
/// [`NetGate`], and other confined transports built on
/// [`connect_upstream_async`]. Built on first use: two workers, since a
/// relay only moves bytes and waits on sockets, and the daemon profile's
/// bounded blocking pool, which is where name resolution runs.
///
/// # Errors
///
/// Returns the error that kept the runtime from starting.
pub fn egress_runtime() -> io::Result<&'static tokio::runtime::Handle> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tilo_rt::daemon("netgate")
                .workers(2)
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map(tokio::runtime::Runtime::handle)
        .map_err(|e| io::Error::other(e.clone()))
}

/// A listening socket [`accept_bounded`] can serve.
trait Acceptor: Send + 'static {
    type Conn: Send + 'static;
    fn accept_conn(&self) -> impl Future<Output = io::Result<Self::Conn>> + Send;
}

impl Acceptor for tokio::net::UnixListener {
    type Conn = UnixStream;
    async fn accept_conn(&self) -> io::Result<UnixStream> {
        self.accept().await.map(|(stream, _)| stream)
    }
}

impl Acceptor for tokio::net::TcpListener {
    type Conn = tokio::net::TcpStream;
    async fn accept_conn(&self) -> io::Result<tokio::net::TcpStream> {
        self.accept().await.map(|(stream, _)| stream)
    }
}

/// Accept connections and serve each as its own task, at most `limit`
/// at once. A slot is claimed *before* accepting, so at the limit the
/// loop stops calling `accept` and new peers queue in the kernel's
/// listen backlog — backpressure, not unbounded growth. Returns when
/// `stop` resolves (its sender sent or dropped).
async fn accept_bounded<L, S, F>(
    listener: L,
    limit: usize,
    mut stop: oneshot::Receiver<()>,
    mut serve: S,
) where
    L: Acceptor,
    S: FnMut(L::Conn) -> F + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let slots = Arc::new(Semaphore::new(limit.max(1)));
    loop {
        let slot = tokio::select! {
            _ = &mut stop => return,
            slot = Arc::clone(&slots).acquire_owned() => match slot {
                Ok(slot) => slot,
                Err(_) => return,
            },
        };
        let conn = tokio::select! {
            _ = &mut stop => return,
            conn = listener.accept_conn() => conn,
        };
        match conn {
            Ok(conn) => {
                let task = serve(conn);
                tokio::spawn(async move {
                    task.await;
                    drop(slot);
                });
            }
            Err(e) => tracing::debug!(error = %e, "accepting a connection failed"),
        }
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

/// The largest HTTP proxy request head the gate reads before giving up.
const MAX_HTTP_HEAD: usize = 16 * 1024;

/// Bound one handshake read or write by [`HANDSHAKE_TIMEOUT`].
async fn within_handshake<T>(io: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, io)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "proxy handshake timed out"))?
}

/// The parsers' view of a handshake in progress: the bytes received so
/// far as input, and a buffer collecting what the parser answers.
struct Exchange<'a> {
    input: io::Cursor<&'a [u8]>,
    output: Vec<u8>,
}

impl Read for Exchange<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

impl Write for Exchange<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A client connection during the proxy handshake, with the bytes it has
/// sent that no parser has consumed yet.
///
/// The SOCKS5 parsers above are written against a blocking `Read + Write`.
/// The gate runs them unchanged over what has arrived so far: a parser
/// that runs out of input fails with `UnexpectedEof` and is re-run once
/// more bytes arrive. They are pure functions of their input, so a re-run
/// is a replay; what a parser writes is sent once, when it finishes.
struct Handshake<'c, C> {
    client: &'c mut C,
    received: Vec<u8>,
}

impl<C: AsyncRead + AsyncWrite + Unpin> Handshake<'_, C> {
    async fn read_more(&mut self) -> io::Result<usize> {
        let mut chunk = [0u8; 4096];
        let n =
            within_handshake(tokio::io::AsyncReadExt::read(&mut *self.client, &mut chunk)).await?;
        self.received
            .extend_from_slice(chunk.get(..n).unwrap_or_default());
        Ok(n)
    }

    async fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        within_handshake(tokio::io::AsyncWriteExt::write_all(
            &mut *self.client,
            bytes,
        ))
        .await
    }

    async fn socks5_reply(&mut self, code: u8) -> io::Result<()> {
        let mut out = Vec::new();
        write_reply(&mut out, code)?;
        self.send(&out).await
    }

    /// Run `parse` over the received bytes, reading more for as long as
    /// it runs out, then send its answer and drop what it consumed.
    async fn run<T>(
        &mut self,
        parse: impl Fn(&mut Exchange<'_>) -> io::Result<T>,
    ) -> io::Result<T> {
        loop {
            let (result, consumed, output) = {
                let mut exchange = Exchange {
                    input: io::Cursor::new(self.received.as_slice()),
                    output: Vec::new(),
                };
                let result = parse(&mut exchange);
                (result, exchange.input.position(), exchange.output)
            };
            match result {
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    if self.read_more().await? == 0 {
                        return Err(e);
                    }
                }
                result => {
                    self.send(&output).await?;
                    let consumed = usize::try_from(consumed)
                        .unwrap_or(usize::MAX)
                        .min(self.received.len());
                    self.received.drain(..consumed);
                    return result;
                }
            }
        }
    }

    /// Read an HTTP message head, up to and including the blank line. A
    /// peer that closes mid-head leaves what it sent as the head.
    async fn http_head(&mut self) -> io::Result<String> {
        loop {
            if let Some(end) = self.received.windows(4).position(|w| w == b"\r\n\r\n") {
                let head: Vec<u8> = self.received.drain(..end.saturating_add(4)).collect();
                return head_text(head);
            }
            if self.received.len() > MAX_HTTP_HEAD {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP proxy request head too large",
                ));
            }
            if self.read_more().await? == 0 {
                return head_text(std::mem::take(&mut self.received));
            }
        }
    }
}

fn head_text(head: Vec<u8>) -> io::Result<String> {
    String::from_utf8(head).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP proxy request is not UTF-8",
        )
    })
}

async fn serve_connection<C: AsyncRead + AsyncWrite + Unpin>(
    mut client: C,
    policy: &EgressPolicy,
    stats: &GateStats,
) -> io::Result<()> {
    let mut handshake = Handshake {
        client: &mut client,
        received: Vec::new(),
    };
    if handshake.read_more().await? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "client closed before the handshake",
        ));
    }
    let first = handshake.received.first().copied().unwrap_or_default();
    let upstream = match detect_protocol(first) {
        Protocol::Socks5 => open_socks5(&mut handshake, policy, stats).await?,
        Protocol::HttpConnect => open_http(&mut handshake, policy, stats).await?,
    };
    let Some(mut upstream) = upstream else {
        return Ok(());
    };
    // Past the handshake the stream is a bulk pipe, with no timeout that
    // would kill a long-lived idle connection. Bytes the client sent
    // ahead of the handshake's answer go first.
    let Handshake {
        received: early, ..
    } = handshake;
    if !early.is_empty() {
        tokio::io::AsyncWriteExt::write_all(&mut upstream, &early).await?;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

async fn open_socks5<C: AsyncRead + AsyncWrite + Unpin>(
    handshake: &mut Handshake<'_, C>,
    policy: &EgressPolicy,
    stats: &GateStats,
) -> io::Result<Option<tokio::net::TcpStream>> {
    handshake
        .run(|exchange: &mut Exchange<'_>| socks5_greet(exchange))
        .await?;
    let dest = handshake
        .run(|exchange: &mut Exchange<'_>| socks5_request(exchange))
        .await?;
    let key = dest.host_key();

    // THE decision. Everything before this is parsing and everything
    // after is plumbing.
    if !policy.permits(&key) {
        stats.denied.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            host = %key,
            port = dest.port(),
            allow = ?policy.describe(),
            "egress DENIED: the app asked for a host it was not granted"
        );
        // `HOST_UNREACHABLE`, not `NOT_ALLOWED`: told there's no route (a state
        // it handles) rather than that it lacks a permission.
        handshake.socks5_reply(reply::HOST_UNREACHABLE).await?;
        return Ok(None);
    }

    // Resolution happens here, after the policy decision and per
    // connection — never cached, never pinned.
    let upstream = match connect_upstream_async(&dest, policy).await {
        Ok(s) => s,
        Err(e) => {
            stats.failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(host = %key, error = %e, "egress allowed but upstream connect failed");
            handshake.socks5_reply(reply::HOST_UNREACHABLE).await?;
            return Ok(None);
        }
    };

    stats.allowed.fetch_add(1, Ordering::Relaxed);
    tracing::info!(host = %key, port = dest.port(), "egress allowed");
    handshake.socks5_reply(reply::SUCCESS).await?;
    Ok(Some(upstream))
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

async fn open_http<C: AsyncRead + AsyncWrite + Unpin>(
    handshake: &mut Handshake<'_, C>,
    policy: &EgressPolicy,
    stats: &GateStats,
) -> io::Result<Option<tokio::net::TcpStream>> {
    const BAD_GATEWAY: &[u8] =
        b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    let head = handshake.http_head().await?;
    let Some(req) = parse_http_proxy_head(&head) else {
        stats.failed.fetch_add(1, Ordering::Relaxed);
        let _ = handshake.send(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        return Ok(None);
    };
    let key = req.dest.host_key();

    if !policy.permits(&key) {
        stats.denied.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            host = %key,
            port = req.dest.port(),
            allow = ?policy.describe(),
            "egress DENIED: the app asked for a host it was not granted"
        );
        // `502`, not `403`: same reasoning as the SOCKS5 reply code — makes a
        // refused host indistinguishable from an unreachable one.
        let _ = handshake.send(BAD_GATEWAY).await;
        return Ok(None);
    }

    let mut upstream = match connect_upstream_async(&req.dest, policy).await {
        Ok(s) => s,
        Err(e) => {
            stats.failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(host = %key, error = %e, "egress allowed but upstream connect failed");
            let _ = handshake.send(BAD_GATEWAY).await;
            return Ok(None);
        }
    };

    stats.allowed.fetch_add(1, Ordering::Relaxed);
    tracing::info!(host = %key, port = req.dest.port(), tunnel = req.tunnel, "egress allowed");

    if req.tunnel {
        handshake
            .send(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    } else {
        tokio::io::AsyncWriteExt::write_all(&mut upstream, &req.forward).await?;
    }
    Ok(Some(upstream))
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

/// Whether a resolved candidate address may be connected to: one that is
/// this device is dropped unless the policy named the host exactly. Runs
/// on the **resolved** address, since a name says nothing about where it
/// points.
fn admit_resolved(addr: SocketAddr, key: &str, policy: &EgressPolicy) -> io::Result<()> {
    if policy.permits_device_local(key) || !is_device_local(addr.ip()) {
        return Ok(());
    }
    tracing::warn!(
        host = %key,
        addr = %addr.ip(),
        allow = ?policy.describe(),
        "egress DROPPED: the host resolves to this device, and the grant does not name \
         it exactly. Device-local services are behind the same decision as the rest of \
         the network."
    );
    Err(io::Error::from_raw_os_error(libc::ENETUNREACH))
}

fn no_addresses() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "name resolved to no addresses")
}

/// Resolve and connect, dropping any candidate address that turns out
/// to be this device unless the policy named the host exactly.
/// Connect an already parsed destination after applying the exact same
/// post-DNS device-local protection used by [`NetGate`].  Other confined
/// transports must use this (or [`connect_upstream_async`]) rather than
/// resolving a permitted name themselves.
///
/// # Errors
///
/// Returns an error if DNS resolution fails, every resolved address is
/// rejected by policy, or no permitted address can be connected to.
pub fn connect_upstream(dest: &Destination, policy: &EgressPolicy) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = match dest {
        Destination::Domain(host, port) => (host.as_str(), *port).to_socket_addrs()?.collect(),
        Destination::Addr(ip, port) => vec![SocketAddr::new(*ip, *port)],
    };
    let key = dest.host_key();
    let mut last = no_addresses();
    for addr in addrs {
        if let Err(e) = admit_resolved(addr, &key, policy) {
            last = e;
            continue;
        }
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// [`connect_upstream`] for async callers: the same resolution after the
/// policy decision and the same device-local protection, without holding
/// a thread through the connect.
///
/// # Errors
///
/// As [`connect_upstream`].
pub async fn connect_upstream_async(
    dest: &Destination,
    policy: &EgressPolicy,
) -> io::Result<tokio::net::TcpStream> {
    let addrs: Vec<SocketAddr> = match dest {
        Destination::Domain(host, port) => tokio::net::lookup_host((host.as_str(), *port))
            .await?
            .collect(),
        Destination::Addr(ip, port) => vec![SocketAddr::new(*ip, *port)],
    };
    let key = dest.host_key();
    let mut last = no_addresses();
    for addr in addrs {
        if let Err(e) = admit_resolved(addr, &key, policy) {
            last = e;
            continue;
        }
        match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = e,
            Err(_) => last = io::Error::new(io::ErrorKind::TimedOut, "upstream connect timed out"),
        }
    }
    Err(last)
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
    serve_relay(
        std::net::TcpListener::bind(("127.0.0.1", port))?,
        gate_socket,
    )
}

/// Serve the relay on an already bound `listener`, so a caller can
/// connect as soon as this is called. At most [`MAX_CONNECTIONS`] are
/// carried at once, each as a task on a one-worker runtime owned by the
/// calling thread. Returns only on error.
///
/// # Errors
///
/// Returns an error if the listener can't be registered with the runtime
/// or the runtime can't start.
pub fn serve_relay(listener: std::net::TcpListener, gate_socket: &Path) -> io::Result<()> {
    serve_relay_limited(listener, gate_socket, MAX_CONNECTIONS)
}

fn serve_relay_limited(
    listener: std::net::TcpListener,
    gate_socket: &Path,
    max_connections: usize,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let gate: Arc<Path> = Arc::from(gate_socket);
    tilo_rt::daemon("netgate-relay")
        .workers(1)
        .run(async move {
            let listener = tokio::net::TcpListener::from_std(listener)?;
            // Never sent: the relay serves until the sandbox is torn down.
            let (_forever, stop) = oneshot::channel();
            accept_bounded(listener, max_connections, stop, move |mut tcp| {
                let gate = Arc::clone(&gate);
                async move {
                    let Ok(mut unix) = UnixStream::connect(&*gate).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
                }
            })
            .await;
            Ok(())
        })
        .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(hosts: &[&str]) -> Vec<String> {
        hosts.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn egress_policy_without_grants_follows_the_manifest() {
        // Phase `a_manifest_allowlist_is_enforced_instead_of_being_dropped`.
        {
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
        // Phase `a_host_manifest_with_no_grants_stays_unrestricted`.
        {
            // The ten apps whose endpoint is user data keep working
            // exactly as before when the user has granted nothing.
            assert_eq!(
                EgressDecision::resolve("host", &[], &[]),
                EgressDecision::Unrestricted
            );
        }
        // Phase `an_unknown_mode_is_blocked_not_shared`.
        {
            for spelling in ["allow-list", "Off", "none", "allowlist ", ""] {
                assert_eq!(
                    EgressDecision::resolve(spelling, &["a.example".into()], &[]),
                    EgressDecision::Blocked,
                    "{spelling:?} is not a policy this launcher knows; it must not share the host network"
                );
            }
        }
        // Phase `no_manifest_network_and_no_grant_is_blocked`.
        {
            assert_eq!(
                EgressDecision::resolve("off", &[], &[]),
                EgressDecision::Blocked
            );
        }
        // Phase `dns_only_does_not_fall_through_to_unrestricted`.
        {
            // The canonical manifest vocabulary has a `dns-only` mode, answered
            // explicitly rather than falling into "not off means unrestricted".
            assert_eq!(
                EgressDecision::resolve("dns-only", &[], &[]),
                EgressDecision::Blocked
            );
        }
    }

    #[test]
    fn grants_narrow_but_never_widen_the_manifest_allowlist() {
        // Phase `a_specific_grant_narrows_a_host_manifest`.
        {
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
        // Phase `a_star_grant_is_unrestricted`.
        {
            assert_eq!(
                EgressDecision::resolve("host", &[], &v(&["*"])),
                EgressDecision::Unrestricted
            );
            assert_eq!(
                EgressDecision::resolve("off", &[], &v(&["*"])),
                EgressDecision::Unrestricted
            );
        }
        // Phase `a_star_grant_narrows_to_an_explicit_manifest_allowlist`.
        {
            // "Allow everything this app asks for" must mean the declared
            // endpoints, not the whole internet.
            let weather = v(&["api.open-meteo.com", "geocoding-api.open-meteo.com"]);
            let decision = EgressDecision::resolve("allowlist", &weather, &v(&["*"]));
            let policy = decision
                .policy()
                .unwrap_or_else(|| panic!("expected the declared endpoints, got {decision:?}"));
            assert_eq!(policy.describe(), weather);
            assert!(
                !decision.shares_host_network(),
                "a `*` grant must not widen a package that enumerated its endpoints: {decision:?}"
            );
        }
        // Phase `a_suffix_grant_selects_the_declared_endpoints_under_it`.
        {
            let maps = v(&[
                "tile.openstreetmap.org",
                "nominatim.openstreetmap.org",
                "overpass-api.de",
            ]);
            let decision = EgressDecision::resolve("allowlist", &maps, &v(&[".openstreetmap.org"]));
            let policy = decision.policy().expect("filtered");
            assert_eq!(
                policy.describe(),
                ["nominatim.openstreetmap.org", "tile.openstreetmap.org"]
            );
            assert!(
                !policy.permits("overpass-api.de"),
                "an endpoint outside the granted suffix must not survive: {policy:?}"
            );
        }
        // Phase `a_grant_cannot_exceed_an_explicit_manifest_allowlist`.
        {
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
        // Phase `a_grant_outside_the_manifest_allowlist_leaving_nothing_blocks`.
        {
            let decision = EgressDecision::resolve(
                "allowlist",
                &v(&["api.open-meteo.com"]),
                &v(&["exfil.example.com"]),
            );
            assert_eq!(decision, EgressDecision::Blocked);
        }
        // Phase `an_address_literal_never_matches_a_hostname_grant`.
        {
            // The bypass this closes: resolving the allowed name yourself then
            // asking the gate for the address would make every name-scoped grant
            // an any-host grant.
            let policy = EgressPolicy::from_grants(["mail.example.com"]);
            assert!(policy.permits("mail.example.com"));
            assert!(
                !policy.permits("93.184.216.34"),
                "an address literal must not satisfy a hostname allowlist entry"
            );
            // An entry that IS a literal is not a host in the one grammar
            // (`collab_types::caps::NetHostPattern`): refused, so it covers
            // nothing, not even itself — the same answer an environment gets.
            let by_addr = EgressPolicy::from_grants(["93.184.216.34"]);
            assert!(!by_addr.permits("93.184.216.34"));
            assert_eq!(by_addr, EgressPolicy::none());
        }
        // Phase `subdomain_grants_use_the_shared_matcher`.
        {
            // `.example.com` is `host_covers`' suffix form, the same spelling the
            // compositor evaluates — asserted so a matcher change shows up here.
            let policy = EgressPolicy::from_grants([".example.com"]);
            assert!(policy.permits("example.com"));
            assert!(policy.permits("mail.example.com"));
            assert!(!policy.permits("example.com.evil.net"));
            assert!(!policy.permits("notexample.com"));
        }
    }

    #[test]
    fn a_star_dot_grant_covers_the_domain_for_apps() {
        // `*.example.com` is how environments, the permissions UI and users
        // spell a domain grant. Read as an exact name it covered nothing, and
        // the app looked offline.
        let decision = EgressDecision::resolve("host", &[], &v(&["*.example.com"]));
        let policy = decision
            .policy()
            .unwrap_or_else(|| panic!("a domain grant must be filtered, got {decision:?}"));
        assert!(policy.permits("a.example.com"));
        assert!(policy.permits("example.com"));
        assert!(!policy.permits("example.com.evil.net"));
        assert!(!decision.shares_host_network());
        // And it caps a manifest allowlist like the `.` spelling does.
        let decision = EgressDecision::resolve(
            "allowlist",
            &v(&["imap.fastmail.com", "evil.example"]),
            &v(&["*.fastmail.com"]),
        );
        let policy = decision
            .policy()
            .unwrap_or_else(|| panic!("expected the covered endpoint, got {decision:?}"));
        assert_eq!(policy.describe(), ["imap.fastmail.com"]);
        // Legacy spellings read the same: case and a trailing dot.
        let decision = EgressDecision::resolve("off", &[], &v(&["Example.COM."]));
        assert!(decision.policy().is_some_and(|p| p.permits("example.com")));
    }

    #[test]
    fn an_address_grant_is_refused_for_apps() {
        // Hosts are granted by name, for apps as for environments: an IP
        // literal grant is ignored (with a warning), so it narrows. Every
        // grant refused means no network, not the manifest's.
        for mode in ["off", "host"] {
            assert_eq!(
                EgressDecision::resolve(mode, &[], &v(&["10.0.0.5"])),
                EgressDecision::Blocked,
                "manifest {mode}"
            );
            assert_eq!(
                EgressDecision::resolve(mode, &[], &v(&["[::1]"])),
                EgressDecision::Blocked,
                "manifest {mode}"
            );
        }
        let decision = EgressDecision::resolve("off", &[], &v(&["192.168.1.1", "example.com"]));
        let policy = decision
            .policy()
            .unwrap_or_else(|| panic!("the named grant must survive, got {decision:?}"));
        assert_eq!(policy.describe(), ["example.com"]);
        assert!(!policy.permits("192.168.1.1"));
    }

    /// The one place apps and environments still differ, on purpose: `*`
    /// keeps `--share-net` for an app, so it reaches this device's loopback;
    /// an environment's `*` keeps a namespace of its own. The grammar and the
    /// policy are the same (`EgressPolicy::Any`); the backend is not. Apps
    /// whose manifests say `host` (Music, Mail) take OAuth redirects on a
    /// loopback listener the host's browser must reach, and the gate carries
    /// TCP only. See `docs/architecture/security.md`.
    #[test]
    fn a_star_grant_keeps_the_host_network_for_apps_deliberately() {
        let decision = EgressDecision::resolve("off", &[], &v(&["*"]));
        assert_eq!(decision, EgressDecision::Unrestricted);
        assert!(decision.shares_host_network());
        assert_eq!(EgressPolicy::from_grants(["*"]), EgressPolicy::Any);
        // A gate standing in for `*` must not be stricter than the
        // `--share-net` it stands in for.
        assert!(EgressPolicy::Any.permits_device_local("localhost"));
    }

    /// Apps and environments answer every row of the shared vector file
    /// identically: this is the app side, through the production decision
    /// (`tilo-egress`'s `environment_egress_answers_every_shared_vector` is
    /// the other).
    #[test]
    fn app_egress_answers_every_shared_vector() {
        let vectors = include_str!("../../../collab/collab-types/tests/net_host_vectors.txt");
        let rows: Vec<(&str, &str, bool)> = vectors
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(
                |line| match line.split_whitespace().collect::<Vec<_>>()[..] {
                    [grant, host, "yes"] => (grant, host, true),
                    [grant, host, "no"] => (grant, host, false),
                    _ => panic!("malformed vector line {line:?}"),
                },
            )
            .collect();
        assert!(rows.len() > 20, "the vector file was read: {rows:?}");
        for (grant, host, covered) in rows {
            let decision = EgressDecision::resolve("off", &[], &v(&[grant]));
            let reaches = match &decision {
                EgressDecision::Blocked => false,
                EgressDecision::Unrestricted => true,
                EgressDecision::Filtered(policy) => policy.permits(host),
            };
            assert_eq!(
                reaches, covered,
                "an app granted {grant:?} asking for {host:?}: {decision:?}"
            );
        }
    }

    #[test]
    fn the_gate_relay_bounds_its_resources_and_forwards_early_bytes() {
        // Phase `gate_socket_path_is_short_enough_for_sun_path`.
        {
            // 108 bytes including the NUL. A path over the limit cannot be
            // connected to, and the failure surfaces far from its cause.
            let p = gate_socket_path(1000, 4_194_304);
            assert!(
                p.as_os_str().len() < 100,
                "gate socket path {p:?} is too long for sun_path"
            );
            assert!(SANDBOX_GATE_PATH.len() < 100);
        }
        // Phase `the_gate_serves_no_more_than_its_connection_limit_at_once`.
        {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("gate.sock");
            let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = service.local_addr().unwrap().port();
            let gate = NetGate::start_limited(&socket, EgressPolicy::from_grants(["localhost"]), 1)
                .unwrap();

            // The one slot: a tunnel that stays open.
            let mut first = std::os::unix::net::UnixStream::connect(&socket).unwrap();
            first.set_read_timeout(Some(SERVED_WITHIN)).unwrap();
            assert_eq!(socks_connect_localhost(&mut first, port), reply::SUCCESS);
            let (held, _) = service.accept().unwrap();

            // The kernel queues a second client, but the gate leaves it there.
            let mut second = std::os::unix::net::UnixStream::connect(&socket).unwrap();
            second.write_all(&[0x05, 0x01, 0x00]).unwrap();
            second.set_read_timeout(Some(NOT_SERVED_WITHIN)).unwrap();
            assert_not_answered(&mut second, "a second gate client");

            // Closing the tunnel frees the slot, and the waiting client is served.
            drop(first);
            drop(held);
            second.set_read_timeout(Some(SERVED_WITHIN)).unwrap();
            let mut greeting = [0u8; 2];
            second.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting, [0x05, 0x00]);
            assert_eq!(gate.stats().allowed(), 1);
        }
        // Phase `the_relay_carries_no_more_than_its_connection_limit_at_once`.
        {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("gate.sock");
            // A stand-in gate that echoes, one thread per connection.
            let echo = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            std::thread::spawn(move || {
                for stream in echo.incoming() {
                    let Ok(stream) = stream else { continue };
                    std::thread::spawn(move || {
                        let (mut r, mut w) = (&stream, &stream);
                        let _ = io::copy(&mut r, &mut w);
                    });
                }
            });
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let relay = listener.local_addr().unwrap();
            std::thread::spawn(move || serve_relay_limited(listener, &socket, 1));

            let mut first = TcpStream::connect(relay).unwrap();
            first.set_read_timeout(Some(SERVED_WITHIN)).unwrap();
            first.write_all(b"a").unwrap();
            let mut byte = [0u8; 1];
            first.read_exact(&mut byte).unwrap();
            assert_eq!(&byte, b"a");

            let mut second = TcpStream::connect(relay).unwrap();
            second.write_all(b"b").unwrap();
            second.set_read_timeout(Some(NOT_SERVED_WITHIN)).unwrap();
            assert_not_answered(&mut second, "a second relay client");

            drop(first);
            second.set_read_timeout(Some(SERVED_WITHIN)).unwrap();
            second.read_exact(&mut byte).unwrap();
            assert_eq!(&byte, b"b");
        }
        // Phase `bytes_sent_ahead_of_the_socks5_reply_reach_the_upstream`.
        {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("gate.sock");
            let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = service.local_addr().unwrap().port();
            let _gate = NetGate::start(&socket, EgressPolicy::from_grants(["localhost"])).unwrap();

            // Greeting, request and payload in one write, as an optimistic
            // client sends them.
            let mut all = vec![0x05, 0x01, 0x00, 0x05, 0x01, 0x00, 0x03, 9];
            all.extend_from_slice(b"localhost");
            all.extend_from_slice(&port.to_be_bytes());
            all.extend_from_slice(b"PING");
            let mut client = std::os::unix::net::UnixStream::connect(&socket).unwrap();
            client.set_read_timeout(Some(SERVED_WITHIN)).unwrap();
            client.write_all(&all).unwrap();

            let (mut upstream, _) = service.accept().unwrap();
            upstream.set_read_timeout(Some(SERVED_WITHIN)).unwrap();
            let mut payload = [0u8; 4];
            upstream.read_exact(&mut payload).unwrap();
            assert_eq!(&payload, b"PING");

            let mut replies = [0u8; 12];
            client.read_exact(&mut replies).unwrap();
            assert_eq!(&replies[..2], &[0x05, 0x00]);
            assert_eq!(replies[3], reply::SUCCESS);
        }
    }

    #[test]
    fn the_loopback_gate_tunnels_granted_hosts_and_refuses_the_rest() {
        let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = service.local_addr().unwrap().port();
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = service.accept() {
                let _ = s.write_all(b"LOCAL-SERVICE");
            }
        });
        let gate = LoopbackGate::start(EgressPolicy::from_grants(["localhost"])).unwrap();
        assert!(gate.local_addr().ip().is_loopback());
        let connect = |target: &str| {
            let mut client = TcpStream::connect(gate.local_addr()).unwrap();
            client.set_read_timeout(Some(SERVED_WITHIN)).unwrap();
            client
                .write_all(format!("CONNECT {target} HTTP/1.1\r\n\r\n").as_bytes())
                .unwrap();
            let mut answer = Vec::new();
            let _ = client.read_to_end(&mut answer);
            String::from_utf8_lossy(&answer).into_owned()
        };

        let granted = connect(&format!("localhost:{port}"));
        assert!(
            granted.starts_with("HTTP/1.1 200") && granted.ends_with("LOCAL-SERVICE"),
            "{granted}"
        );
        let refused = connect(&format!("127.0.0.1:{port}"));
        assert!(refused.starts_with("HTTP/1.1 502"), "{refused}");
        assert_eq!((gate.stats().allowed(), gate.stats().denied()), (1, 1));
    }

    #[test]
    fn the_proxy_environment_names_the_gate_for_every_client_convention() {
        let env = proxy_env(9050);
        let get = |name: &str| {
            env.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.as_str())
        };
        for var in ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"] {
            assert_eq!(get(var), Some("http://127.0.0.1:9050"));
        }
        for var in ["ALL_PROXY", "all_proxy"] {
            assert_eq!(get(var), Some("socks5h://127.0.0.1:9050"));
        }
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
    fn socks5_greetings_and_requests_parse_or_are_refused() {
        // Phase `the_first_byte_picks_the_protocol`.
        {
            assert_eq!(detect_protocol(0x05), Protocol::Socks5);
            // Every HTTP method starts with an uppercase letter.
            for m in *b"CGPH" {
                assert_eq!(detect_protocol(m), Protocol::HttpConnect);
            }
        }
        // Phase `greeting_rejects_a_non_socks5_peer`.
        {
            let mut p = pipe(vec![0x04, 0x01]);
            assert!(socks5_greet(&mut p).is_err());
        }
        // Phase `parses_a_domain_connect_request`.
        {
            let mut req = vec![0x05, 0x01, 0x00, 0x03, 11];
            req.extend_from_slice(b"example.com");
            req.extend_from_slice(&443u16.to_be_bytes());
            let mut p = pipe(req);
            assert_eq!(
                socks5_request(&mut p).unwrap(),
                Destination::Domain("example.com".to_string(), 443)
            );
        }
        // Phase `parses_an_ipv4_connect_request`.
        {
            let mut req = vec![0x05, 0x01, 0x00, 0x01, 198, 51, 100, 9];
            req.extend_from_slice(&80u16.to_be_bytes());
            let mut p = pipe(req);
            let dest = socks5_request(&mut p).unwrap();
            assert_eq!(dest.host_key(), "198.51.100.9");
            assert_eq!(dest.port(), 80);
        }
        // Phase `a_non_connect_command_is_refused_with_a_reply`.
        {
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
    }

    #[test]
    fn http_proxy_heads_parse_to_the_requested_authority() {
        // Phase `connect_carries_the_name_the_client_asked_for`.
        {
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
        // Phase `connect_without_a_port_defaults_to_443`.
        {
            let req =
                parse_http_proxy_head("CONNECT example.com HTTP/1.1\r\n\r\n").expect("parses");
            assert_eq!(
                req.dest,
                Destination::Domain("example.com".to_string(), 443)
            );
        }
        // Phase `an_absolute_form_request_is_rewritten_to_origin_form`.
        {
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
        // Phase `an_ipv6_literal_authority_parses`.
        {
            let req = parse_http_proxy_head("CONNECT [2001:db8::1]:8443 HTTP/1.1\r\n\r\n")
                .expect("parses");
            assert_eq!(
                req.dest,
                Destination::Domain("2001:db8::1".to_string(), 8443)
            );
            // And it is still matched as a literal, so it cannot satisfy a
            // hostname grant.
            let policy = EgressPolicy::from_grants(["example.com"]);
            assert!(!policy.permits(&req.dest.host_key()));
        }
        // Phase `a_malformed_head_parses_to_nothing_rather_than_a_default_host`.
        {
            assert!(parse_http_proxy_head("").is_none());
            assert!(parse_http_proxy_head("GARBAGE\r\n\r\n").is_none());
            // An absolute-form URI with a scheme we do not proxy must not
            // silently become a hostname.
            assert!(parse_http_proxy_head("GET ftp://example.com/x HTTP/1.1\r\n\r\n").is_none());
        }
    }

    // Device-local egress.

    #[test]
    fn device_local_destinations_need_an_exact_or_wildcard_grant() {
        // Phase `loopback_and_link_local_are_device_local_in_every_spelling`.
        {
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
        // Phase `a_public_address_is_not_device_local`.
        {
            // Reserved for documentation (RFC 5737 / 3849), so no machine
            // running this test holds them.
            for addr in ["198.51.100.9", "203.0.113.7", "2001:db8::1"] {
                assert!(
                    !is_device_local(addr.parse().unwrap()),
                    "{addr} is not this device"
                );
            }
        }
        // Phase `this_hosts_own_addresses_are_device_local_even_when_they_look_public`.
        {
            // Every address the host holds routes to its own services; loopback
            // is assertable without knowing the network.
            assert!(is_device_local("127.0.0.1".parse().unwrap()));
            // The mechanism itself: an address the host does not hold cannot
            // be bound.
            assert!(!is_assigned_to_this_host("198.51.100.9".parse().unwrap()));
        }
        // Phase `only_an_exact_grant_or_the_wildcard_reaches_this_device`.
        {
            let suffix = EgressPolicy::from_grants([".localhost"]);
            assert!(
                suffix.permits("localhost"),
                "the suffix pattern does cover the name — the access decision is unchanged"
            );
            assert!(
                !suffix.permits_device_local("localhost"),
                "but covering a name is not naming it, so it must not reach the device itself"
            );

            let exact = EgressPolicy::from_grants(["localhost"]);
            assert!(exact.permits_device_local("localhost"));

            // A wildcard allowlist becomes `--share-net` in production; the gate
            // must not be stricter than the sandbox it stands in for.
            assert!(EgressPolicy::from_grants(["*"]).permits_device_local("localhost"));

            // An exact grant for one host does not carry device-local access
            // for a different one.
            assert!(!exact.permits_device_local("other.localhost"));
        }
        // Phase `a_suffix_grant_cannot_reach_a_loopback_service_an_exact_grant_can`.
        // The production connect path, against a real listener on the host's
        // loopback — the shape a local service actually has.
        {
            let listener =
                std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback service");
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                while let Ok((mut s, _)) = listener.accept() {
                    let _ = s.write_all(b"LOCAL-SERVICE");
                }
            });

            let dest = Destination::Domain("localhost".to_string(), port);

            let suffix = EgressPolicy::from_grants([".localhost"]);
            let err = connect_upstream(&dest, &suffix)
                .expect_err("a suffix grant must not reach this device");
            assert_eq!(
                err.raw_os_error(),
                Some(libc::ENETUNREACH),
                "and the failure must look like an absence of network, not a refusal: {err}"
            );

            let exact = EgressPolicy::from_grants(["localhost"]);
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

    /// Long enough that a connection the gate would serve has been
    /// served; only used to show that one it must not serve wasn't.
    const NOT_SERVED_WITHIN: Duration = Duration::from_millis(300);
    /// A bound on a wait that is expected to succeed, so a regression
    /// fails the test rather than hanging it.
    const SERVED_WITHIN: Duration = Duration::from_secs(10);

    fn assert_not_answered(stream: &mut impl Read, what: &str) {
        let mut byte = [0u8; 1];
        match stream.read(&mut byte) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            other => panic!("{what} was answered past the connection limit: {other:?}"),
        }
    }

    /// SOCKS5 CONNECT to `localhost:port` through the gate at `socket`;
    /// returns the reply code.
    fn socks_connect_localhost(stream: &mut std::os::unix::net::UnixStream, port: u16) -> u8 {
        stream.write_all(&[0x05, 0x01, 0x00]).unwrap();
        let mut greeting = [0u8; 2];
        stream.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting, [0x05, 0x00]);
        let mut request = vec![0x05, 0x01, 0x00, 0x03, 9];
        request.extend_from_slice(b"localhost");
        request.extend_from_slice(&port.to_be_bytes());
        stream.write_all(&request).unwrap();
        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).unwrap();
        reply[1]
    }
}
