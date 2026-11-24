//! Per-host egress filtering for a sandboxed app.
//!
//! The namespace stays empty and the only way out is a socket the proxy holds on the
//! host side, so an app that ignores the proxy is offline, not unfiltered. The proxy
//! matches the name in the request against the allowlist and resolves per connection:
//! no address is pinned, an address literal never matches a hostname entry, and
//! device-local addresses are dropped after resolution unless the allowlist names them
//! exactly.
//!
//! A granted call's audio and video ride the same proxy: a SOCKS5 UDP association to
//! public addresses only, open only while the daemon says a call of the app is live.

use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub use collab_types::caps::{EgressPolicy, NetHostPattern};
use tilo_socket::tokio::Stream as LocalStream;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Semaphore, oneshot};

/// How long a client gets for each read and write of the handshake, and how long the proxy waits on an
/// upstream `connect`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How many connections one proxy, or one relay, carries at once.
pub const MAX_CONNECTIONS: usize = 256;

/// The loopback port the in-sandbox relay listens on.
pub const RELAY_PORT: u16 = 9050;

/// Where the proxy socket is bind-mounted inside the sandbox.
pub const SANDBOX_PROXY_PATH: &str = "/run/tilo/net/proxy.sock";

/// The name of the file a Windows appliance presentation keeps in its launch's keeper
/// directory: the guest's endpoint and token, from which the desktop that adopts the launch after
/// a restart serves its egress again.
pub const APPLIANCE_NETWORK: &str = "appliance-network";

/// Host-side path the proxy listens on.
pub fn proxy_socket_path(runtime: &Path, launcher_pid: u32) -> PathBuf {
    runtime.join(format!("tilo/net/{launcher_pid}.sock"))
}

/// What outbound access an app ends up with, after the packager's manifest and the user's grants have
/// both been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressDecision {
    Blocked,
    /// Unrestricted outbound access — `--share-net`, no proxy.
    Unrestricted,
    /// A specific set of hosts: the namespace stays empty and a proxy carries the traffic.
    Filtered(EgressPolicy),
}

/// What a launch's egress amounts to: the TCP decision and whether it may
/// send calls' UDP while a call of the app is live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressAccess {
    pub decision: EgressDecision,
    pub calls: bool,
}

impl EgressAccess {
    /// Whether a proxy carries anything for this launch: a host allowlist, or calls with no raw
    /// host network (a `*` grant already reaches UDP directly).
    pub fn needs_proxy(&self) -> bool {
        self.decision.policy().is_some() || (self.calls && !self.decision.shares_host_network())
    }
}

impl EgressDecision {
    pub fn shares_host_network(&self) -> bool {
        matches!(self, EgressDecision::Unrestricted)
    }

    pub fn policy(&self) -> Option<&EgressPolicy> {
        match self {
            EgressDecision::Filtered(p) => Some(p),
            _ => None,
        }
    }

    /// Resolve the packager's declaration and the person's grants into one decision: what both
    /// allow. The manifest names the most an app may ask for (`host` asks for anything) and a
    /// `NetHost` grant is what allows it, so neither reaches the network alone. The one exception
    /// is the image's own apps (`image_shipped`), reviewed with the image: until a person narrows
    /// one with a grant, its declaration stands.
    pub fn resolve(
        net_mode: &str,
        manifest_hosts: &[String],
        granted_hosts: &[String],
        image_shipped: bool,
    ) -> Self {
        Self::resolve_tcp(net_mode, manifest_hosts, granted_hosts, image_shipped)
    }

    fn resolve_tcp(
        net_mode: &str,
        manifest_hosts: &[String],
        granted_hosts: &[String],
        image_shipped: bool,
    ) -> Self {
        let declared = match net_mode {
            // A manifest's `*.domain` is every name under it, not the domain. A wildcard naming a
            // public suffix would reach every name someone else may register, so it declares
            // nothing; the manifest read refuses it, and an older manifest is narrowed here.
            "allowlist" => EgressPolicy::from_declared(manifest_hosts.iter().filter_map(|host| {
                match tilo_app_package::validate::public_suffix_wildcard(host) {
                    Some(domain) => {
                        tracing::warn!(
                            %domain,
                            "ignoring a declared wildcard whose domain is a public suffix"
                        );
                        None
                    }
                    None => Some(host.as_str()),
                }
            })),
            "host" => EgressPolicy::Any,
            "off" | "dns-only" => return EgressDecision::Blocked,
            // A spelling this launcher does not know is not a policy it can enforce.
            other => {
                tracing::warn!(
                    mode = other,
                    "unknown [sandbox.net].mode; the app gets no network"
                );
                return EgressDecision::Blocked;
            }
        };
        if image_shipped && granted_hosts.is_empty() {
            return decision_of(declared);
        }
        // Read through the one grammar: a grant that is not a host (an IP literal, junk) is dropped
        // with a warning, so it narrows.
        let granted = EgressPolicy::from_grants(granted_hosts.iter().map(String::as_str));
        decision_of(declared.intersect(&granted))
    }
}

/// What outbound access a policy alone amounts to: nothing, everything (`*`, which for an app is
/// `--share-net`), or a proxy.
fn decision_of(policy: EgressPolicy) -> EgressDecision {
    match policy {
        EgressPolicy::Any => EgressDecision::Unrestricted,
        EgressPolicy::Hosts(ref hosts) if hosts.is_empty() => EgressDecision::Blocked,
        filtered @ EgressPolicy::Hosts(_) => EgressDecision::Filtered(filtered),
    }
}

/// The outbound access this launch of the package at `payload_root` resolves
/// to: what its manifest declares and what the person granted it, read through
/// [`EgressDecision::resolve`], whichever launcher enforces it.
pub fn egress_decision(
    manifest: &crate::manifest::ManifestView,
    capabilities: &crate::capabilities::CapabilityPlan,
    payload_root: &Path,
) -> EgressDecision {
    egress_access(manifest, capabilities, payload_root).decision
}

/// [`egress_decision`] with the calls grant: a package that declares calls and a person who
/// granted them, or an image's own app before anyone narrowed it.
pub fn egress_access(
    manifest: &crate::manifest::ManifestView,
    capabilities: &crate::capabilities::CapabilityPlan,
    payload_root: &Path,
) -> EgressAccess {
    let image_shipped = package_store::layout::image_shipped(payload_root);
    // The declaration stands for an image's own app, as its hosts do, until a person here answers
    // for its network; anyone else needs their own calls grant.
    let calls = manifest.sandbox.net.calls
        && (capabilities.calls || (image_shipped && capabilities.net_hosts.is_empty()));
    let decision = EgressDecision::resolve_tcp(
        &manifest.sandbox.net.mode,
        &manifest.sandbox.net.hosts,
        &capabilities.net_hosts,
        image_shipped,
    );
    // Applied here, not at the check: the namespace decision and the check must come from the same
    // answer, or the app gets an empty namespace with nothing to reach through it.
    if matches!(decision, EgressDecision::Filtered(_))
        && package_store::dev_env::dev_override(crate::dev_env::EGRESS_PROXY_ENV)
            .is_some_and(|v| v == "off")
    {
        tracing::warn!(
            hosts = ?decision.policy().map(EgressPolicy::describe),
            "{}=off: launching with UNRESTRICTED outbound network access even though this app \
             was granted specific hosts. Per-host filtering is NOT enforced for this launch.",
            crate::dev_env::EGRESS_PROXY_ENV,
        );
        return EgressAccess {
            decision: EgressDecision::Unrestricted,
            calls,
        };
    }
    EgressAccess { decision, calls }
}

/// Counts of what the proxy decided, so a test can assert on the decision the production path actually
/// took rather than on argv.
#[derive(Debug, Default)]
pub struct ProxyStats {
    pub allowed: AtomicU64,
    pub denied: AtomicU64,
    pub failed: AtomicU64,
    /// Datagrams a live call carried, and datagrams dropped by the calls rule.
    pub udp_allowed: AtomicU64,
    pub udp_denied: AtomicU64,
    /// Destinations a live call opened a flow to.
    pub udp_flows: AtomicU64,
}

impl ProxyStats {
    pub fn allowed(&self) -> u64 {
        self.allowed.load(Ordering::Relaxed)
    }
    pub fn denied(&self) -> u64 {
        self.denied.load(Ordering::Relaxed)
    }
    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn udp_allowed(&self) -> u64 {
        self.udp_allowed.load(Ordering::Relaxed)
    }

    pub fn udp_denied(&self) -> u64 {
        self.udp_denied.load(Ordering::Relaxed)
    }

    pub fn udp_flows(&self) -> u64 {
        self.udp_flows.load(Ordering::Relaxed)
    }
}

/// Errors from [`EgressProxy::start`].
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("creating the proxy socket directory {path}: {source}")]
    Mkdir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "binding the egress proxy socket at {path}: {source}. The app was granted access to \
         specific hosts, which can only be enforced by brokering its traffic through this \
         socket; without it the sandbox would have to be given unrestricted network access \
         instead, which is not what was granted."
    )]
    Bind {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("starting the runtime that serves the egress proxy: {0}")]
    Runtime(#[source] io::Error),
}

/// A proxy's policy as it stands, and a signal that cuts every open connection when it changes:
/// a person who takes a grant back ends the traffic it allowed, not only the next connection.
#[derive(Debug)]
pub struct LivePolicy {
    policy: std::sync::RwLock<EgressPolicy>,
    cut: tokio::sync::broadcast::Sender<()>,
}

impl LivePolicy {
    #[must_use]
    pub fn new(policy: EgressPolicy) -> Self {
        let (cut, _) = tokio::sync::broadcast::channel(1);
        Self {
            policy: std::sync::RwLock::new(policy),
            cut,
        }
    }

    /// The policy a connection is decided against now.
    #[must_use]
    pub fn policy(&self) -> EgressPolicy {
        self.policy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replace the policy, and cut every connection that was open under the old one.
    pub fn replace(&self, policy: EgressPolicy) {
        *self
            .policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = policy;
        // A send nobody hears has cut nothing, and that is right: no connection was open.
        let _ = self.cut.send(());
    }

    /// News of the next cut.
    fn cut_signal(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.cut.subscribe()
    }
}

/// A running proxy.
#[derive(Debug)]
pub struct EgressProxy {
    socket_path: PathBuf,
    control_path: Option<PathBuf>,
    stats: Arc<ProxyStats>,
    policy: Arc<LivePolicy>,
    /// The sandbox end of the datagram pair, until the launch hands it to its relay.
    udp_client: Option<std::sync::Arc<std::os::unix::net::UnixDatagram>>,
    /// Dropping a sender ends the loop it stops.
    _stops: Vec<oneshot::Sender<()>>,
}

impl EgressProxy {
    /// Bind `socket_path` and serve SOCKS5 against `policy` until dropped.
    pub fn start(socket_path: &Path, policy: EgressPolicy) -> Result<EgressProxy, ProxyError> {
        Self::start_for_app_with(socket_path, policy, "", false, false, MAX_CONNECTIONS)
    }

    /// [`Self::start`] for `app`, with the calls grant the launch resolved.
    pub fn start_for_app(
        socket_path: &Path,
        policy: EgressPolicy,
        app: &str,
        calls: bool,
    ) -> Result<EgressProxy, ProxyError> {
        Self::start_for_app_with(socket_path, policy, app, calls, false, MAX_CONNECTIONS)
    }

    /// [`Self::start_for_app`] with a test's own loopback echo permitted, and a bound on connections.
    pub fn start_for_app_with(
        socket_path: &Path,
        policy: EgressPolicy,
        app: &str,
        calls: bool,
        test_loopback: bool,
        max_connections: usize,
    ) -> Result<EgressProxy, ProxyError> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ProxyError::Mkdir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // A socket left behind by a crashed launcher would make bind() fail with EADDRINUSE.
        let _ = std::fs::remove_file(socket_path);

        let bind_error = |source| ProxyError::Bind {
            path: socket_path.to_path_buf(),
            source,
        };
        let listener = tilo_files::native::socket::bind(socket_path).map_err(bind_error)?;
        let runtime = egress_runtime().map_err(ProxyError::Runtime)?;
        let listener = {
            let _entered = runtime.enter();
            tilo_socket::tokio::listener(listener).map_err(bind_error)?
        };

        let stats = Arc::new(ProxyStats::default());
        let mut stops = Vec::new();
        let (calls_state, udp_client) = if calls {
            let (proxy_end, sandbox_end) =
                std::os::unix::net::UnixDatagram::pair().map_err(ProxyError::Runtime)?;
            proxy_end
                .set_nonblocking(true)
                .map_err(ProxyError::Runtime)?;
            let ingress = {
                let _entered = runtime.enter();
                tokio::net::UnixDatagram::from_std(proxy_end).map_err(ProxyError::Runtime)?
            };
            let (cut, _) = tokio::sync::broadcast::channel(1);
            let calling = Arc::new(CallsState {
                app: app.to_owned(),
                granted: true,
                live: AtomicBool::new(false),
                association: AtomicU64::new(0),
                next_association: AtomicU64::new(1),
                cut,
                flows: std::sync::Mutex::new(std::collections::HashMap::new()),
                stats: Arc::clone(&stats),
                test_loopback,
                ingress: Arc::new(ingress),
            });
            stops.push(serve_udp(Arc::clone(&calling))?);
            let control_path = control_socket_path(socket_path);
            let control = serve_control(&control_path, Arc::clone(&calling))?;
            stops.push(control);
            // The daemon writes the lane before it tells the proxies, and this socket is bound, so
            // a launch that starts during a live calling reads it here, and one that starts after
            // gets its line on the socket.
            if socket_path
                .parent()
                .and_then(|directory| std::fs::read_to_string(lane_path(directory)).ok())
                .is_some_and(|lane| lane.trim() == app)
            {
                calling.live.store(true, Ordering::Relaxed);
            }
            (Some(calling), Some(std::sync::Arc::new(sandbox_end)))
        } else {
            (None, None)
        };
        let control_path = calls_state
            .as_ref()
            .map(|_| control_socket_path(socket_path));

        tracing::info!(
            socket = %socket_path.display(),
            hosts = ?policy.describe(),
            calls = calls_state.is_some(),
            "egress proxy listening; the sandbox netns stays empty and this socket is its \
             only route out"
        );
        let policy = Arc::new(LivePolicy::new(policy));
        let udp_bound = SocketAddr::from(([127, 0, 0, 1], RELAY_PORT));
        let (served_stats, stop) = serve_proxy(
            runtime,
            listener,
            Arc::clone(&policy),
            max_connections,
            Arc::clone(&stats),
            calls_state,
            udp_bound,
        );
        stops.push(stop);
        let stats = served_stats;
        Ok(EgressProxy {
            socket_path: socket_path.to_path_buf(),
            control_path,
            stats,
            policy,
            udp_client,
            _stops: stops,
        })
    }

    /// [`Self::start_for_app_with`] for a unit test that needs a bound on connections.
    #[cfg(test)]
    fn start_limited(
        socket_path: &Path,
        policy: EgressPolicy,
        max_connections: usize,
    ) -> Result<EgressProxy, ProxyError> {
        Self::start_for_app_with(socket_path, policy, "", false, false, max_connections)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn stats(&self) -> &Arc<ProxyStats> {
        &self.stats
    }

    /// Where the daemon says a call of this app is live, when the app has calls.
    pub fn control_path(&self) -> Option<&Path> {
        self.control_path.as_deref()
    }

    /// The sandbox end of the datagram pair, for the launch to hand its relay.
    pub fn take_udp_client(&mut self) -> Option<std::sync::Arc<std::os::unix::net::UnixDatagram>> {
        self.udp_client.take()
    }

    /// Replace the policy this proxy enforces, cutting the connections allowed under the old one.
    pub fn set_policy(&self, policy: EgressPolicy) {
        self.policy.replace(policy);
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        // `_stops` is dropped right after this, which ends the loops they stop.
        let _ = std::fs::remove_file(&self.socket_path);
        if let Some(control) = &self.control_path {
            let _ = std::fs::remove_file(control);
        }
    }
}

/// A proxy on a loopback TCP port, for a host with no network namespace to leave empty (macOS): there
/// the sandbox profile admits connections to this one port, and the proxy environment names it.
#[derive(Debug)]
pub struct LoopbackProxy {
    addr: SocketAddr,
    stats: Arc<ProxyStats>,
    _stop: oneshot::Sender<()>,
}

impl LoopbackProxy {
    /// Bind a free port on `127.0.0.1` and serve `policy` on it until dropped.
    pub fn start(policy: EgressPolicy) -> Result<LoopbackProxy, ProxyError> {
        let bind_error = |source| ProxyError::Bind {
            path: PathBuf::from("127.0.0.1:0"),
            source,
        };
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(bind_error)?;
        let addr = listener.local_addr().map_err(bind_error)?;
        listener.set_nonblocking(true).map_err(bind_error)?;
        let runtime = egress_runtime().map_err(ProxyError::Runtime)?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener).map_err(bind_error)?
        };
        tracing::info!(%addr, hosts = ?policy.describe(), "loopback egress proxy listening");
        let policy = Arc::new(LivePolicy::new(policy));
        let (stats, stop) = serve_proxy(
            runtime,
            listener,
            policy,
            MAX_CONNECTIONS,
            Arc::new(ProxyStats::default()),
            None,
            addr,
        );
        Ok(LoopbackProxy {
            addr,
            stats,
            _stop: stop,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stats(&self) -> &Arc<ProxyStats> {
        &self.stats
    }
}

/// Serve every connection `listener` accepts against `policy` on `runtime`, until the returned sender
/// is dropped.
fn serve_proxy<L>(
    runtime: &tokio::runtime::Handle,
    listener: L,
    policy: Arc<LivePolicy>,
    max_connections: usize,
    stats: Arc<ProxyStats>,
    calls: Option<Arc<CallsState>>,
    udp_bound: SocketAddr,
) -> (Arc<ProxyStats>, oneshot::Sender<()>)
where
    L: Acceptor,
    L::Conn: AsyncRead + AsyncWrite + Unpin,
{
    let (stop, stopped) = oneshot::channel();
    let served = Arc::clone(&stats);
    runtime.spawn(accept_bounded(
        listener,
        max_connections,
        stopped,
        move |client| {
            let policy = Arc::clone(&policy);
            let stats = Arc::clone(&served);
            let calls = calls.clone();
            async move {
                if let Err(e) = serve_connection(client, &policy, &stats, calls, udp_bound).await {
                    tracing::debug!(error = %e, "egress proxy connection ended");
                }
            }
        },
    ));
    (stats, stop)
}

/// Where a calling app's proxy is told a call of it is live, beside the proxy socket.
#[must_use]
pub fn control_socket_path(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("ctl.sock")
}

/// A datagram a call's relay takes as "a new association starts": the app's next UDP socket is a
/// new client, and the relay forgets the last one.
const UDP_RELAY_RESET: &[u8] = &[0xff];

/// The calls lane of one launch: whether the daemon says a call of this app is live, the one UDP
/// association open on it, and each destination it carries, as one audited flow.
#[derive(Debug)]
struct CallsState {
    app: String,
    /// The person granted calls and the package declared them.
    granted: bool,
    /// The daemon said a call of this app is live.
    live: AtomicBool,
    /// The association a datagram belongs to, or zero when none is open.
    association: AtomicU64,
    next_association: AtomicU64,
    /// Cuts every UDP flow open when the call or the association ends.
    cut: tokio::sync::broadcast::Sender<()>,
    flows: std::sync::Mutex<std::collections::HashMap<SocketAddr, Arc<UdpFlow>>>,
    stats: Arc<ProxyStats>,
    /// A test's own setup may carry to loopback, where its echo server lives. Production never
    /// sets it: Discord's voice servers are public addresses.
    test_loopback: bool,
    /// The sandbox relay's end of the datagram pair: the only way into a call's UDP.
    ingress: Arc<tokio::net::UnixDatagram>,
}

/// One destination a live call sends to, and the address replies must come back from.
#[derive(Debug)]
struct UdpFlow {
    socket: Arc<tokio::net::UdpSocket>,
}

impl CallsState {
    /// End every flow the call had: an association ending, or the daemon saying the call ended.
    fn cut_flows(&self) {
        self.flows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        // Nobody hearing the cut has no flow to end.
        let _ = self.cut.send(());
    }

    fn deny(&self, reason: &str, host: &str) {
        self.stats.udp_denied.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(app = %self.app, %host, reason, "udp DENIED");
    }

    fn allowed(&self) {
        self.stats.udp_allowed.fetch_add(1, Ordering::Relaxed);
    }

    /// Tell the relay a new association starts, so it takes the next source as its client.
    async fn reset_relay(&self) {
        let _ = self.ingress.send(UDP_RELAY_RESET).await;
    }
}

/// Serve the daemon's call-lane signal for this launch: `calls <app> on|off`, from a process of
/// this user (only the session can reach this directory). The app itself never says a call is
/// live.
fn serve_control(path: &Path, state: Arc<CallsState>) -> Result<oneshot::Sender<()>, ProxyError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ProxyError::Mkdir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let bind_error = |source| ProxyError::Bind {
        path: path.to_path_buf(),
        source,
    };
    let _ = std::fs::remove_file(path);
    let listener = tilo_files::native::socket::bind(path).map_err(bind_error)?;
    listener.set_nonblocking(true).map_err(bind_error)?;
    let runtime = egress_runtime().map_err(ProxyError::Runtime)?;
    let listener = {
        let _entered = runtime.enter();
        tokio::net::UnixListener::from_std(listener).map_err(bind_error)?
    };
    let (stop, mut stopped) = oneshot::channel();
    runtime.spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = &mut stopped => return,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, _)) = accepted else { continue };
            let state = Arc::clone(&state);
            tokio::spawn(async move { control_connection(stream, &state).await });
        }
    });
    Ok(stop)
}

async fn control_connection(mut stream: tokio::net::UnixStream, state: &CallsState) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let Ok(cred) = stream.peer_cred() else { return };
    if cred.uid() != rustix::process::geteuid().as_raw() {
        tracing::warn!("refused a calls control connection of another user");
        return;
    }
    let mut buf = [0u8; 512];
    let Ok(n) = stream.read(&mut buf).await else {
        return;
    };
    let line = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
    let words: Vec<&str> = line.split_whitespace().collect();
    let ["calls", app, switch] = words.as_slice() else {
        let _ = stream.write_all(b"unknown\n").await;
        return;
    };
    if *app != state.app {
        let _ = stream.write_all(b"another app\n").await;
        return;
    }
    match *switch {
        "on" => {
            state.live.store(true, Ordering::Relaxed);
            tracing::info!(app = %state.app, "udp lane open while a call is live");
        }
        "off" => {
            state.live.store(false, Ordering::Relaxed);
            state.cut_flows();
            tracing::info!(app = %state.app, "udp lane closed: the call ended");
        }
        _ => {
            let _ = stream.write_all(b"unknown\n").await;
            return;
        }
    }
    let _ = stream.write_all(b"ok\n").await;
}

/// The app whose call is live, written beside the proxies before the daemon tells them: a proxy
/// that starts while a call is already live reads it, and one that starts after gets the line.
fn lane_path(directory: &Path) -> PathBuf {
    directory.join("calls")
}

/// Forget the lane a session that ended left behind: no call outlives the daemon that knew it.
pub fn forget_calls(directory: &Path) {
    let _ = std::fs::remove_file(lane_path(directory));
}

/// Tell every launch's proxy in `directory` whether a call of `app` is live: the daemon says so,
/// never the app. A proxy of another app ignores the line.
pub fn announce_calls(directory: &Path, app: &str, live: bool) {
    let lane = lane_path(directory);
    if live {
        let _ = std::fs::create_dir_all(directory);
        let _ = std::fs::write(&lane, app);
    } else if std::fs::read_to_string(&lane).is_ok_and(|current| current.trim() == app) {
        let _ = std::fs::remove_file(&lane);
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let notice = format!("calls {app} {}\n", if live { "on" } else { "off" });
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".ctl.sock") {
            continue;
        }
        let Ok(mut stream) = tilo_files::native::socket::connect(&entry.path()) else {
            continue;
        };
        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
        let _ = stream.write_all(notice.as_bytes());
    }
}

/// The in-sandbox half of a call's UDP: the app's datagrams from `127.0.0.1:port`, carried to the
/// proxy over `fd`, and only the proxy's replies carried back to the one client that sent first.
/// A datagram from any other source is dropped: this is never a relay open to the sandbox's other
/// processes.
pub fn run_udp_relay(fd: std::os::fd::RawFd, port: u16) -> io::Result<()> {
    use std::os::fd::FromRawFd as _;
    // SAFETY: the launcher passed this end of the pair to this process alone.
    let host = unsafe { std::os::unix::net::UnixDatagram::from_raw_fd(fd) };
    let app = std::net::UdpSocket::bind(("127.0.0.1", port))?;
    let timeout = Some(Duration::from_millis(200));
    host.set_read_timeout(timeout)?;
    app.set_read_timeout(timeout)?;
    let client = Arc::new(std::sync::Mutex::new(None::<SocketAddr>));
    let (to_proxy, from_app) = (host.try_clone()?, app.try_clone()?);
    let pinned = Arc::clone(&client);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 65_535];
        loop {
            match from_app.recv_from(&mut buf) {
                Ok((n, from)) => {
                    // The first sender is pinned; every later datagram must come from it.
                    let from_client = *pinned
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get_or_insert(from)
                        == from;
                    if from_client && let Some(datagram) = buf.get(..n) {
                        let _ = to_proxy.send(datagram);
                    }
                }
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut => {}
                Err(_) => return,
            }
        }
    });
    let mut buf = vec![0u8; 65_535];
    loop {
        match host.recv(&mut buf) {
            // The proxy end is gone: this launch is over, and no app traffic may cross without it.
            Ok(0) => return Ok(()),
            Ok(n) => {
                let Some(datagram) = buf.get(..n) else {
                    continue;
                };
                let mut pinned = client
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                // The proxy's reset forgets the pinned client: the next call's app sends first.
                let to = if datagram == UDP_RELAY_RESET {
                    pinned.take();
                    None
                } else {
                    *pinned
                };
                drop(pinned);
                if let Some(client) = to {
                    let _ = app.send_to(datagram, client);
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            }
            Err(_) => return Ok(()),
        }
    }
}

/// The SOCKS5 UDP request header's destination, and the data that follows it.
fn udp_destination(frame: &[u8]) -> Option<(Destination, &[u8])> {
    // Reserved, reserved, no fragment, then the address's kind.
    let [0, 0, 0, kind, rest @ ..] = frame else {
        return None;
    };
    match kind {
        0x01 => {
            let (ip, rest) = rest.split_first_chunk::<4>()?;
            let (port, data) = udp_port(rest)?;
            Some((Destination::Addr(IpAddr::from(*ip), port), data))
        }
        0x03 => {
            let (len, rest) = rest.split_first()?;
            let (name, rest) = rest.split_at_checked(usize::from(*len))?;
            let name = std::str::from_utf8(name).ok()?.to_owned();
            let (port, data) = udp_port(rest)?;
            Some((Destination::Domain(name, port), data))
        }
        0x04 => {
            let (ip, rest) = rest.split_first_chunk::<16>()?;
            let (port, data) = udp_port(rest)?;
            Some((Destination::Addr(IpAddr::from(*ip), port), data))
        }
        _ => None,
    }
}

/// A big-endian port, and what follows it.
fn udp_port(bytes: &[u8]) -> Option<(u16, &[u8])> {
    let (port, rest) = bytes.split_first_chunk::<2>()?;
    Some((u16::from_be_bytes(*port), rest))
}

/// A SOCKS5 UDP reply frame carrying `data` from `from`.
fn udp_reply(from: SocketAddr, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0, 0, 0];
    match from.ip() {
        IpAddr::V4(v4) => {
            frame.push(0x01);
            frame.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            frame.push(0x04);
            frame.extend_from_slice(&v6.octets());
        }
    }
    frame.extend_from_slice(&from.port().to_be_bytes());
    frame.extend_from_slice(data);
    frame
}

/// May a call send a datagram to `ip`? A public internet address, never this device or a private,
/// link-local, multicast or documentation one. `test_loopback` is a test's own echo server.
fn udp_reachable(ip: IpAddr, test_loopback: bool) -> bool {
    let ip = canonical_ip(ip);
    if ip.is_loopback() {
        return test_loopback;
    }
    if is_device_local(ip) {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || matches!(v4.octets(), [100, 64..128, ..]))
        }
        IpAddr::V6(v6) => {
            !(v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || matches!(v6.segments(), [0x2001, 0x0db8, ..]))
        }
    }
}

/// Serve a calling app's UDP datagrams until stopped.
fn serve_udp(state: Arc<CallsState>) -> Result<oneshot::Sender<()>, ProxyError> {
    let runtime = egress_runtime().map_err(ProxyError::Runtime)?;
    let (stop, mut stopped) = oneshot::channel();
    runtime.spawn(async move {
        let mut buf = vec![0u8; 65_535];
        loop {
            let got = tokio::select! {
                _ = &mut stopped => return,
                got = state.ingress.recv(&mut buf) => got,
            };
            let Some(frame) = got.ok().and_then(|n| buf.get(..n)) else {
                continue;
            };
            handle_udp(&state, frame).await;
        }
    });
    Ok(stop)
}

/// Resolve a datagram's named destination to one public address, within a bound: a call names
/// an address it was handed, and one slow lookup must not hold the lane's other datagrams.
async fn resolve_udp(state: &Arc<CallsState>, name: &str, port: u16) -> Option<SocketAddr> {
    let resolved = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::lookup_host((name, port)),
    )
    .await;
    let mut addrs = match resolved {
        Ok(Ok(addrs)) => addrs,
        Ok(Err(error)) => {
            tracing::warn!(app = %state.app, host = %name, %error, "udp name did not resolve");
            state.deny("the name did not resolve", name);
            return None;
        }
        Err(_) => {
            state.deny("the name did not resolve in time", name);
            return None;
        }
    };
    let found = addrs.find(|addr| udp_reachable(addr.ip(), state.test_loopback));
    if found.is_none() {
        state.deny("the name has no public address", name);
    }
    found
}

/// Open the call's flow to `target`, and carry its replies back to the app while the call lasts.
async fn open_udp_flow(state: &Arc<CallsState>, target: SocketAddr) -> Option<Arc<UdpFlow>> {
    let bind: SocketAddr = if target.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        ([0_u16; 8], 0).into()
    };
    let Ok(socket) = tokio::net::UdpSocket::bind(bind).await else {
        state.deny("could not bind a datagram socket", &target.ip().to_string());
        return None;
    };
    if socket.connect(target).await.is_err() {
        state.deny("could not reach the address", &target.ip().to_string());
        return None;
    }
    let flow = Arc::new(UdpFlow {
        socket: Arc::new(socket),
    });
    state
        .flows
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(target, Arc::clone(&flow));
    state.stats.udp_flows.fetch_add(1, Ordering::Relaxed);
    tracing::info!(
        app = %state.app,
        address = %target,
        "udp flow open; datagrams go only to this address and back while a call is live"
    );
    let reader = Arc::clone(&flow);
    let ingress = Arc::clone(&state.ingress);
    let mut cut = state.cut.subscribe();
    if let Ok(runtime) = egress_runtime() {
        runtime.spawn(async move {
            let mut buf = vec![0u8; 65_535];
            loop {
                let got = tokio::select! {
                    _ = cut.recv() => return,
                    got = reader.socket.recv_from(&mut buf) => got,
                };
                let Ok((n, source)) = got else { continue };
                if source != target {
                    continue;
                }
                if let Some(data) = buf.get(..n) {
                    let _ = ingress.send(&udp_reply(source, data)).await;
                }
            }
        });
    }
    Some(flow)
}

async fn handle_udp(state: &Arc<CallsState>, frame: &[u8]) {
    if state.association.load(Ordering::Relaxed) == 0
        || !state.live.load(Ordering::Relaxed)
        || !state.granted
    {
        state.deny("outside a call", "");
        return;
    }
    let Some((dest, data)) = udp_destination(frame) else {
        state.deny("not a SOCKS5 UDP datagram", "");
        return;
    };
    let target = match dest {
        Destination::Addr(ip, port) => SocketAddr::new(ip, port),
        Destination::Domain(name, port) => {
            let Some(target) = resolve_udp(state, &name, port).await else {
                return;
            };
            target
        }
    };
    if !udp_reachable(target.ip(), state.test_loopback) {
        state.deny("not a public address", &target.ip().to_string());
        return;
    }
    // The relay drops a datagram from a source other than the association's client, so what
    // reaches here is that client's alone.
    let existing = state
        .flows
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&target)
        .cloned();
    let flow = if let Some(flow) = existing {
        flow
    } else {
        let Some(flow) = open_udp_flow(state, target).await else {
            return;
        };
        flow
    };
    let _ = flow.socket.send(data).await;
    state.allowed();
}

/// The proxy environment that sends an ordinary HTTP client through a proxy listening on
/// `127.0.0.1:port`.
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

/// The runtime every egress relay in this process serves on — each [`EgressProxy`], and other confined
/// transports built on [`connect_upstream_async`].
pub fn egress_runtime() -> io::Result<&'static tokio::runtime::Handle> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tilo_rt::daemon("egress_proxy")
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

impl Acceptor for tilo_socket::tokio::Listener {
    type Conn = LocalStream;
    async fn accept_conn(&self) -> io::Result<LocalStream> {
        self.accept().await.map(|(stream, _)| stream)
    }
}

impl Acceptor for tokio::net::TcpListener {
    type Conn = tokio::net::TcpStream;
    async fn accept_conn(&self) -> io::Result<tokio::net::TcpStream> {
        self.accept().await.map(|(stream, _)| stream)
    }
}

/// Accept connections and serve each as its own task, at most `limit` at once.
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
    /// `ATYP=0x03`.
    Domain(String, u16),
    /// `ATYP=0x01` / `0x04`.
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
    /// "connection not allowed by ruleset" — the allowlist said no.
    pub const NOT_ALLOWED: u8 = 0x02;
    pub const HOST_UNREACHABLE: u8 = 0x04;
    pub const COMMAND_NOT_SUPPORTED: u8 = 0x07;
    pub const ADDRESS_TYPE_NOT_SUPPORTED: u8 = 0x08;
}

/// Read the client greeting and answer it.
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

/// A SOCKS5 request the proxy understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocksRequest {
    /// `CONNECT`: one TCP stream to the destination.
    Connect(Destination),
    /// `UDP ASSOCIATE`: the client will send UDP datagrams to the address the reply names, while
    /// this TCP connection stays open.
    Associate(Destination),
}

/// Read a SOCKS5 request, CONNECT or UDP ASSOCIATE.
fn socks5_message<S: Read + Write>(stream: &mut S) -> io::Result<SocksRequest> {
    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    let (ver, cmd, atyp) = (head[0], head[1], head[3]);
    if ver != 0x05 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad SOCKS version",
        ));
    }
    if cmd != 0x01 && cmd != 0x03 {
        // BIND needs a policy story of its own, and nothing Tilo runs asks for it.
        write_reply(stream, reply::COMMAND_NOT_SUPPORTED)?;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("SOCKS5 command {cmd:#04x} is not CONNECT or UDP ASSOCIATE"),
        ));
    }
    let dest = read_destination(stream, atyp)?;
    Ok(if cmd == 0x01 {
        SocksRequest::Connect(dest)
    } else {
        SocksRequest::Associate(dest)
    })
}

/// Read a CONNECT request. A UDP ASSOCIATE is refused here; the proxy reads it through
/// [`socks5_message`], because only a calls grant can carry one.
pub fn socks5_request<S: Read + Write>(stream: &mut S) -> io::Result<Destination> {
    match socks5_message(stream)? {
        SocksRequest::Connect(dest) => Ok(dest),
        SocksRequest::Associate(_) => {
            write_reply(stream, reply::COMMAND_NOT_SUPPORTED)?;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a UDP ASSOCIATE needs a calls grant",
            ))
        }
    }
}

/// The destination of a SOCKS5 request, by address type.
fn read_destination<S: Read + Write>(stream: &mut S, atyp: u8) -> io::Result<Destination> {
    match atyp {
        0x01 => {
            let mut raw = [0u8; 4];
            stream.read_exact(&mut raw)?;
            Ok(Destination::Addr(IpAddr::from(raw), read_port(stream)?))
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            let mut name = vec![0u8; len[0] as usize];
            stream.read_exact(&mut name)?;
            let name = String::from_utf8(name)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "domain is not UTF-8"))?;
            Ok(Destination::Domain(name, read_port(stream)?))
        }
        0x04 => {
            let mut raw = [0u8; 16];
            stream.read_exact(&mut raw)?;
            Ok(Destination::Addr(IpAddr::from(raw), read_port(stream)?))
        }
        other => {
            write_reply(stream, reply::ADDRESS_TYPE_NOT_SUPPORTED)?;
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown SOCKS5 address type {other:#04x}"),
            ))
        }
    }
}

fn read_port<S: Read>(stream: &mut S) -> io::Result<u16> {
    let mut raw = [0u8; 2];
    stream.read_exact(&mut raw)?;
    Ok(u16::from_be_bytes(raw))
}

/// Write a reply header.
pub fn write_reply<S: Write>(stream: &mut S, code: u8) -> io::Result<()> {
    stream.write_all(&[0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
}

/// Write a reply naming the address the client sends to (a UDP association's relay).
pub fn write_reply_with_bound<S: Write>(
    stream: &mut S,
    code: u8,
    bound: SocketAddr,
) -> io::Result<()> {
    let mut reply = vec![0x05, code, 0x00];
    match bound.ip() {
        IpAddr::V4(v4) => {
            reply.push(0x01);
            reply.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            reply.push(0x04);
            reply.extend_from_slice(&v6.octets());
        }
    }
    reply.extend_from_slice(&bound.port().to_be_bytes());
    stream.write_all(&reply)
}

/// Which proxy protocol a client is speaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Socks5,
    HttpConnect,
}

/// SOCKS5's first byte is its version, `0x05`.
pub fn detect_protocol(first_byte: u8) -> Protocol {
    if first_byte == 0x05 {
        Protocol::Socks5
    } else {
        Protocol::HttpConnect
    }
}

/// The largest HTTP proxy request head the proxy reads before giving up.
const MAX_HTTP_HEAD: usize = 16 * 1024;

/// Bound one handshake read or write by [`HANDSHAKE_TIMEOUT`].
async fn within_handshake<T>(io: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, io)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "proxy handshake timed out"))?
}

/// The parsers' view of a handshake in progress: the bytes received so far as input, and a buffer
/// collecting what the parser answers.
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

/// A client connection during the proxy handshake, with the bytes it has sent that no parser has
/// consumed yet.
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

    /// Run `parse` over the received bytes, reading more for as long as it runs out, then send its
    /// answer and drop what it consumed.
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

    /// Read an HTTP message head, up to and including the blank line.
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
    live: &LivePolicy,
    stats: &ProxyStats,
    calls: Option<Arc<CallsState>>,
    udp_bound: SocketAddr,
) -> io::Result<()> {
    // Subscribed before the destination is read, so a policy taken back while the connection is
    // being set up cuts it as surely as one taken back while it carries traffic.
    let mut cut = live.cut_signal();
    let policy = live.policy();
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
        Protocol::Socks5 => {
            open_socks5(&mut handshake, &policy, stats, calls.as_ref(), udp_bound).await?
        }
        Protocol::HttpConnect => open_http(&mut handshake, &policy, stats).await?,
    };
    let Some(mut upstream) = upstream else {
        return Ok(());
    };
    // Past the handshake the stream is a bulk pipe, with no timeout that would kill a long-lived idle
    // connection.
    let Handshake {
        received: early, ..
    } = handshake;
    if !early.is_empty() {
        tokio::io::AsyncWriteExt::write_all(&mut upstream, &early).await?;
    }
    if cut.try_recv().is_ok() {
        return Ok(());
    }
    tokio::select! {
        _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
        _ = cut.recv() => {
            tracing::info!("egress connection cut: the policy changed under it");
        }
    }
    Ok(())
}

async fn open_socks5<C: AsyncRead + AsyncWrite + Unpin>(
    handshake: &mut Handshake<'_, C>,
    policy: &EgressPolicy,
    stats: &ProxyStats,
    calls: Option<&Arc<CallsState>>,
    udp_bound: SocketAddr,
) -> io::Result<Option<tokio::net::TcpStream>> {
    handshake
        .run(|exchange: &mut Exchange<'_>| socks5_greet(exchange))
        .await?;
    let request = handshake
        .run(|exchange: &mut Exchange<'_>| socks5_message(exchange))
        .await?;
    let dest = match request {
        SocksRequest::Connect(dest) => dest,
        SocksRequest::Associate(_) => {
            // A UDP association is the calls grant's alone, and only while a call of this app is
            // live. Its datagrams go to `udp_bound`, and the association lasts as long as this
            // control connection.
            let Some(calls) = calls.filter(|calls| calls.granted) else {
                stats.udp_denied.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("udp DENIED: the app has no calls grant");
                handshake.socks5_reply(reply::NOT_ALLOWED).await?;
                return Ok(None);
            };
            if !calls.live.load(Ordering::Relaxed) {
                stats.udp_denied.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(app = %calls.app, "udp DENIED: no call of this app is live");
                handshake.socks5_reply(reply::NOT_ALLOWED).await?;
                return Ok(None);
            }
            calls.cut_flows();
            let id = calls.next_association.fetch_add(1, Ordering::Relaxed);
            calls.association.store(id, Ordering::Relaxed);
            calls.reset_relay().await;
            handshake
                .run(|exchange: &mut Exchange<'_>| {
                    write_reply_with_bound(exchange, reply::SUCCESS, udp_bound)
                })
                .await?;
            tracing::info!(app = %calls.app, "udp association open for a live call");
            // The control connection carries nothing itself: its end is the association's end.
            let mut sink = [0u8; 512];
            while let Ok(n) = tokio::io::AsyncReadExt::read(&mut *handshake.client, &mut sink).await
            {
                if n == 0 {
                    break;
                }
            }
            let _ = calls
                .association
                .compare_exchange(id, 0, Ordering::Relaxed, Ordering::Relaxed);
            calls.cut_flows();
            return Ok(None);
        }
    };
    let key = dest.host_key();

    // The policy decision; resolution happens after it, per connection.
    if !policy.permits(&key) {
        stats.denied.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            host = %key,
            port = dest.port(),
            allow = ?policy.describe(),
            "egress DENIED: the app asked for a host it was not granted"
        );
        // `HOST_UNREACHABLE`, not `NOT_ALLOWED`: told there's no route (a state it handles) rather
        // than that it lacks a permission.
        handshake.socks5_reply(reply::HOST_UNREACHABLE).await?;
        return Ok(None);
    }

    // Resolution happens here, after the policy decision and per connection — never cached, never
    // pinned.
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
    /// The destination, taken from the `CONNECT` target or the absolute-form request URI.
    pub dest: Destination,
    /// True for `CONNECT` (a tunnel), false for an ordinary proxied request that must be forwarded.
    pub tunnel: bool,
    /// For a forwarded request, the bytes to send upstream: the request line rewritten to origin-form
    /// plus the original headers.
    pub forward: Vec<u8>,
}

/// Parse an HTTP proxy request: `CONNECT host:port` or an absolute-URI GET.
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

    // Absolute-form URI; origin servers aren't required to accept it, so rewritten before forwarding.
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
    stats: &ProxyStats,
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
        // `502`, not `403`: same reasoning as the SOCKS5 reply code — makes a refused host
        // indistinguishable from an unreachable one.
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

/// Fold an address to its v4 form for the device-local test, so one spelling of loopback can't slip
/// past a test for another.
fn canonical_ip(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => v6.to_ipv4().map_or(addr, IpAddr::V4),
        IpAddr::V4(_) => addr,
    }
}

/// Is `addr` an address of *this device*?
pub fn is_device_local(addr: IpAddr) -> bool {
    // Before folding: `Ipv6Addr::to_ipv4` maps `::1` to `0.0.0.1`, so the v6 loopback has to be
    // recognised as itself first.
    if addr.is_loopback() || addr.is_unspecified() {
        return true;
    }
    let addr = canonical_ip(addr);
    if addr.is_loopback() || addr.is_unspecified() {
        return true;
    }
    let link_local = match addr {
        IpAddr::V4(v4) => v4.is_link_local(),
        // `fe80::/10`, spelled out because `Ipv6Addr::is_unicast_link_local` is not stable.
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    };
    link_local || is_assigned_to_this_host(addr)
}

/// Does this machine hold `addr` on one of its interfaces?
fn is_assigned_to_this_host(addr: IpAddr) -> bool {
    std::net::UdpSocket::bind((addr, 0)).is_ok()
}

/// Whether a resolved candidate address may be connected to: one that is this device is dropped unless
/// the policy named the host exactly.
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

/// Connect a parsed destination, dropping candidates that are this device unless the policy named the
/// host.
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

/// The in-sandbox relay: accept on loopback, carry each connection to the proxy socket verbatim.
pub fn run_relay(proxy_socket: &Path, port: u16) -> io::Result<()> {
    serve_relay(
        std::net::TcpListener::bind(("127.0.0.1", port))?,
        proxy_socket,
    )
}

/// Serve the relay on an already bound `listener`, so a caller can connect as soon as this is called.
pub fn serve_relay(listener: std::net::TcpListener, proxy_socket: &Path) -> io::Result<()> {
    serve_relay_limited(listener, proxy_socket, MAX_CONNECTIONS)
}

fn serve_relay_limited(
    listener: std::net::TcpListener,
    proxy_socket: &Path,
    max_connections: usize,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let proxy: Arc<Path> = Arc::from(proxy_socket);
    tilo_rt::daemon("egress_proxy-relay")
        .workers(1)
        .run(async move {
            let listener = tokio::net::TcpListener::from_std(listener)?;
            // Never sent: the relay serves until the sandbox is torn down.
            let (_forever, stop) = oneshot::channel();
            accept_bounded(listener, max_connections, stop, move |mut tcp| {
                let proxy = Arc::clone(&proxy);
                async move {
                    let Ok(mut unix) = tilo_files::native::socket::connect(&proxy)
                        .and_then(tilo_socket::tokio::from_std)
                    else {
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
    use std::net::TcpStream;

    fn v(hosts: &[&str]) -> Vec<String> {
        hosts.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn grants_narrow_but_never_widen_the_manifest_allowlist() {
        {
            // "Allow everything this app asks for" means its declared endpoints, not the whole
            // internet.
            let weather = v(&["api.open-meteo.com", "geocoding-api.open-meteo.com"]);
            let decision = EgressDecision::resolve("allowlist", &weather, &v(&["*"]), false);
            let policy = decision
                .policy()
                .unwrap_or_else(|| panic!("expected the declared endpoints, got {decision:?}"));
            assert_eq!(policy.describe(), weather);
            assert!(
                !decision.shares_host_network(),
                "a `*` grant must not widen a package that enumerated its endpoints: {decision:?}"
            );
        }
        {
            let decision = EgressDecision::resolve(
                "allowlist",
                &v(&["api.open-meteo.com"]),
                &v(&["exfil.example.com"]),
                false,
            );
            assert_eq!(decision, EgressDecision::Blocked);
        }
        {
            // Resolving the allowed name and then asking for the address would make every name grant
            // an any-host grant.
            let policy = EgressPolicy::from_grants(["mail.example.com"]);
            assert!(policy.permits("mail.example.com"));
            assert!(
                !policy.permits("93.184.216.34"),
                "an address literal must not satisfy a hostname allowlist entry"
            );
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

    tilo_test_env::scenario! {
        fn the_proxy_speaks_socks5_and_http_proxy_to_its_clients;

        phase the_proxy_environment_names_the_proxy_for_every_client_convention;
        {
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

        phase socks5_greetings_and_requests_parse_or_are_refused;
        {
            {
                assert_eq!(detect_protocol(0x05), Protocol::Socks5);
                for m in *b"CGPH" {
                    assert_eq!(detect_protocol(m), Protocol::HttpConnect);
                }
            }
            {
                let mut p = pipe(vec![0x04, 0x01]);
                assert!(socks5_greet(&mut p).is_err());
            }
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
            {
                let mut req = vec![0x05, 0x01, 0x00, 0x01, 198, 51, 100, 9];
                req.extend_from_slice(&80u16.to_be_bytes());
                let mut p = pipe(req);
                let dest = socks5_request(&mut p).unwrap();
                assert_eq!(dest.host_key(), "198.51.100.9");
                assert_eq!(dest.port(), 80);
            }
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

        phase http_proxy_heads_parse_to_the_requested_authority;
        {
            {
                // The property that matters: an HTTP CONNECT names a host, so the policy sees a name, same
                // as socks5h, without a client feature flag.
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
            {
                let req =
                    parse_http_proxy_head("CONNECT example.com HTTP/1.1\r\n\r\n").expect("parses");
                assert_eq!(
                    req.dest,
                    Destination::Domain("example.com".to_string(), 443)
                );
            }
            {
                // A plain-HTTP proxied request arrives with an absolute URI, which origin servers aren't
                // required to accept, so it's rewritten first.
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
            {
                let req = parse_http_proxy_head("CONNECT [2001:db8::1]:8443 HTTP/1.1\r\n\r\n")
                    .expect("parses");
                assert_eq!(
                    req.dest,
                    Destination::Domain("2001:db8::1".to_string(), 8443)
                );
                // And it is still matched as a literal, so it cannot satisfy a hostname grant.
                let policy = EgressPolicy::from_grants(["example.com"]);
                assert!(!policy.permits(&req.dest.host_key()));
            }
            {
                assert!(parse_http_proxy_head("").is_none());
                assert!(parse_http_proxy_head("GARBAGE\r\n\r\n").is_none());
                // An absolute-form URI with a scheme we do not proxy must not silently become a hostname.
                assert!(parse_http_proxy_head("GET ftp://example.com/x HTTP/1.1\r\n\r\n").is_none());
            }
        }
    }

    // Device-local egress.

    tilo_test_env::scenario! {
        fn egress_follows_the_manifest_narrowed_by_grants;

        phase without_a_grant_only_the_image_s_own_apps_reach_the_network;
        {
            let maps = v(&[
                "tile.openstreetmap.org",
                "nominatim.openstreetmap.org",
                "overpass-api.de",
                "router.project-osrm.org",
                "api.transitous.org",
            ]);
            {
                // An app the person installed has no network until they grant it, whatever its
                // manifest declares.
                assert_eq!(
                    EgressDecision::resolve("allowlist", &maps, &[], false),
                    EgressDecision::Blocked
                );
                assert_eq!(
                    EgressDecision::resolve("host", &[], &[], false),
                    EgressDecision::Blocked
                );
            }
            {
                // The image's own copy reaches what it declares.
                let decision = EgressDecision::resolve("allowlist", &maps, &[], true);
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
                // An image app whose endpoint is user data (a mail server) keeps working.
                assert_eq!(
                    EgressDecision::resolve("host", &[], &[], true),
                    EgressDecision::Unrestricted
                );
                // A person's grant still narrows it.
                let narrowed = EgressDecision::resolve("host", &[], &v(&["imap.example.com"]), true);
                assert_eq!(
                    narrowed.policy().map(EgressPolicy::describe),
                    Some(vec!["imap.example.com".to_owned()])
                );
            }
            {
                for image in [false, true] {
                    for spelling in ["allow-list", "Off", "none", "allowlist ", ""] {
                        assert_eq!(
                            EgressDecision::resolve(spelling, &["a.example".into()], &v(&["*"]), image),
                            EgressDecision::Blocked,
                            "{spelling:?} is not a policy this launcher knows; it must not share the host network"
                        );
                    }
                    // `off` and `dns-only` are answered explicitly, and no grant opens them.
                    for mode in ["off", "dns-only"] {
                        assert_eq!(
                            EgressDecision::resolve(mode, &[], &v(&["*"]), image),
                            EgressDecision::Blocked
                        );
                    }
                }
            }
        }

        phase a_grant_and_the_declaration_meet;
        {
            // A domain grant narrower than a declared domain: what both allow is the grant.
            let decision = EgressDecision::resolve(
                "allowlist",
                &v(&["*.discord.media", "discord.com"]),
                &v(&["eu.discord.media"]),
                false,
            );
            assert_eq!(
                decision.policy().map(EgressPolicy::describe),
                Some(vec!["eu.discord.media".to_owned()])
            );
            // A grant can't widen past the declaration, for an image app either.
            for image in [false, true] {
                let decision = EgressDecision::resolve(
                    "allowlist",
                    &v(&["speed.cloudflare.com"]),
                    &v(&["speed.cloudflare.com", ".example.com"]),
                    image,
                );
                assert_eq!(
                    decision.policy().map(EgressPolicy::describe),
                    Some(vec!["speed.cloudflare.com".to_owned()])
                );
            }
        }

        phase a_star_dot_grant_covers_the_domain_for_apps;
        {
            // `*.example.com` is how environments, the permissions UI and users spell a domain grant.
            let decision = EgressDecision::resolve("host", &[], &v(&["*.example.com"]), false);
            let policy = decision
                .policy()
                .unwrap_or_else(|| panic!("a domain grant must be filtered, got {decision:?}"));
            assert!(policy.permits("a.example.com"));
            assert!(policy.permits("example.com"));
            assert!(!policy.permits("example.com.evil.net"));
            assert!(!decision.shares_host_network());
            let decision = EgressDecision::resolve(
                "allowlist",
                &v(&["imap.fastmail.com", "evil.example"]),
                &v(&["*.fastmail.com"]), false,
            );
            let policy = decision
                .policy()
                .unwrap_or_else(|| panic!("expected the covered endpoint, got {decision:?}"));
            assert_eq!(policy.describe(), ["imap.fastmail.com"]);
            let decision = EgressDecision::resolve("host", &[], &v(&["Example.COM."]), false);
            assert!(decision.policy().is_some_and(|p| p.permits("example.com")));
        }

        phase a_declared_star_dot_domain_covers_only_names_under_it;
        {
            // Discord's voice servers are regional names under one domain. The manifest declares
            // `*.discord.media`; the proxy must reach every name under it and nothing that merely
            // looks like one.
            let declared = v(&["*.discord.media"]);
            let decision = EgressDecision::resolve("allowlist", &declared, &[], true);
            let policy = decision
                .policy()
                .unwrap_or_else(|| panic!("a declared domain must be filtered, got {decision:?}"));
            for host in [
                "rotterdam1234.discord.media",
                "a.b.discord.media",
                "EU.Discord.Media.",
            ] {
                assert!(policy.permits(host), "{host} is under the domain");
            }
            for host in ["discord.media", "discord.media.evil", "xdiscord.media"] {
                assert!(!policy.permits(host), "{host} is not under the domain");
            }
            // An installed app still needs the person's grant; the declaration bounds it.
            assert_eq!(
                EgressDecision::resolve("allowlist", &declared, &[], false),
                EgressDecision::Blocked
            );
            let granted =
                EgressDecision::resolve("allowlist", &declared, &v(&["*.discord.media"]), false);
            let policy = granted
                .policy()
                .unwrap_or_else(|| panic!("a granted domain must be filtered, got {granted:?}"));
            assert!(policy.permits("rotterdam1234.discord.media"));
            for host in ["discord.media", "discord.media.evil", "xdiscord.media"] {
                assert!(
                    !policy.permits(host),
                    "a grant must not widen the declared domain past it: {host}"
                );
            }
            // A wildcard naming a public suffix would reach every name someone else may register:
            // it declares nothing, and a grant with the same spelling does not open it.
            for suffix in ["*.co.uk", "*.github.io"] {
                assert_eq!(
                    EgressDecision::resolve("allowlist", &v(&[suffix]), &[], true),
                    EgressDecision::Blocked,
                    "{suffix} names a public suffix, not a service's domain"
                );
                assert_eq!(
                    EgressDecision::resolve("allowlist", &v(&[suffix]), &v(&[suffix]), false),
                    EgressDecision::Blocked,
                    "a grant must not open a public-suffix wildcard"
                );
            }
        }

        phase an_address_grant_is_refused_for_apps;
        {
            // Hosts are granted by name: an IP literal grant is ignored with a warning, so it narrows.
            for mode in ["off", "host"] {
                assert_eq!(
                    EgressDecision::resolve(mode, &[], &v(&["10.0.0.5"]), false),
                    EgressDecision::Blocked,
                    "manifest {mode}"
                );
                assert_eq!(
                    EgressDecision::resolve(mode, &[], &v(&["[::1]"]), false),
                    EgressDecision::Blocked,
                    "manifest {mode}"
                );
            }
            let decision = EgressDecision::resolve("host", &[], &v(&["192.168.1.1", "example.com"]), false);
            let policy = decision
                .policy()
                .unwrap_or_else(|| panic!("the named grant must survive, got {decision:?}"));
            assert_eq!(policy.describe(), ["example.com"]);
            assert!(!policy.permits("192.168.1.1"));
        }

        phase a_star_grant_keeps_the_host_network_for_apps_deliberately;
        // The one place apps and environments differ: `*` keeps `--share-net` for an app, so it reaches
        // this device's loopback.
        {
            let decision = EgressDecision::resolve("host", &[], &v(&["*"]), false);
            assert_eq!(decision, EgressDecision::Unrestricted);
            assert!(decision.shares_host_network());
            assert_eq!(EgressPolicy::from_grants(["*"]), EgressPolicy::Any);
            // A proxy in place of `*` is not stricter than the `--share-net` it stands in for.
            assert!(EgressPolicy::Any.permits_device_local("localhost"));
        }

        phase app_egress_answers_every_shared_vector;
        // The app side of the shared vector file, through the production decision.
        {
            let vectors = include_str!("../../collab-types/tests/net_host_vectors.txt");
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
                let decision = EgressDecision::resolve("host", &[], &v(&[grant]), false);
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

        phase device_local_destinations_need_an_exact_or_wildcard_grant;
        {
            {
                for addr in [
                    "127.0.0.1",
                    "127.0.0.53",
                    "0.0.0.0",
                    "::1",
                    "::",
                    "169.254.169.254",
                    "fe80::1",
                    // IPv4-mapped and IPv4-compatible loopback.
                    "::ffff:127.0.0.1",
                    "::127.0.0.1",
                ] {
                    assert!(
                        is_device_local(addr.parse().unwrap()),
                        "{addr} is an address of this device"
                    );
                }
            }
            {
                // Reserved for documentation (RFC 5737 / 3849), so no machine running this test holds
                // them.
                for addr in ["198.51.100.9", "203.0.113.7", "2001:db8::1"] {
                    assert!(
                        !is_device_local(addr.parse().unwrap()),
                        "{addr} is not this device"
                    );
                }
            }
            {
                // Every address the host holds routes to its own services; loopback is assertable without
                // knowing the network.
                assert!(is_device_local("127.0.0.1".parse().unwrap()));
                // The mechanism itself: an address the host does not hold cannot be bound.
                assert!(!is_assigned_to_this_host("198.51.100.9".parse().unwrap()));
            }
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

                // A wildcard allowlist becomes `--share-net`, so the proxy is not stricter than the sandbox
                // it stands in for.
                assert!(EgressPolicy::from_grants(["*"]).permits_device_local("localhost"));

                // An exact grant for one host does not carry device-local access for a different one.
                assert!(!exact.permits_device_local("other.localhost"));
            }
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
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let connect = |policy: &EgressPolicy| {
                    runtime
                        .block_on(connect_upstream_async(&dest, policy))
                        .map(|stream| stream.into_std().unwrap())
                };

                let suffix = EgressPolicy::from_grants([".localhost"]);
                let err = connect(&suffix).expect_err("a suffix grant must not reach this device");
                assert_eq!(
                    err.raw_os_error(),
                    Some(libc::ENETUNREACH),
                    "and the failure must look like an absence of network, not a refusal: {err}"
                );

                let exact = EgressPolicy::from_grants(["localhost"]);
                let mut stream =
                    connect(&exact).expect("an exact grant for a device-local host must reach it");
                stream.set_nonblocking(false).unwrap();
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
    }

    /// An acceptor of numbered connections that counts how often the loop asked it for one.
    struct CountedAcceptor {
        asked: Arc<std::sync::atomic::AtomicUsize>,
        queued: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<u32>>,
    }

    impl Acceptor for CountedAcceptor {
        type Conn = u32;
        async fn accept_conn(&self) -> io::Result<u32> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            let next = self.queued.lock().await.recv().await;
            next.ok_or_else(|| io::ErrorKind::BrokenPipe.into())
        }
    }

    tilo_test_env::scenario! {
        fn the_proxy_tunnels_what_is_granted_within_its_bounds;

        phase the_proxy_relay_bounds_its_resources_and_forwards_early_bytes;
        {
            {
                // 108 bytes including the NUL.
                let p = proxy_socket_path(Path::new("/run/user/1000"), 4_194_304);
                assert!(
                    p.as_os_str().len() < 100,
                    "proxy socket path {p:?} is too long for sun_path"
                );
                assert!(SANDBOX_PROXY_PATH.len() < 100);
            }
            {
                let dir = tempfile::tempdir().unwrap();
                let socket = dir.path().join("proxy.sock");
                let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let port = service.local_addr().unwrap().port();
                let proxy = EgressProxy::start_limited(&socket, EgressPolicy::from_grants(["localhost"]), 1)
                    .unwrap();

                // The one slot: a tunnel that stays open.
                let mut first = std::os::unix::net::UnixStream::connect(&socket).unwrap();
                first
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                assert_eq!(socks_connect_localhost(&mut first, port), reply::SUCCESS);
                let (held, _) = service.accept().unwrap();

                // The kernel queues a second client, and the proxy leaves it there
                // (`at_the_limit_the_accept_loop_waits_for_a_free_slot`).
                let mut second = std::os::unix::net::UnixStream::connect(&socket).unwrap();
                second.write_all(&[0x05, 0x01, 0x00]).unwrap();

                drop(first);
                drop(held);
                second
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                let mut greeting = [0u8; 2];
                second.read_exact(&mut greeting).unwrap();
                assert_eq!(greeting, [0x05, 0x00]);
                assert_eq!(proxy.stats().allowed(), 1);
            }
            {
                let dir = tempfile::tempdir().unwrap();
                let socket = dir.path().join("proxy.sock");
                // A stand-in proxy that echoes, one thread per connection.
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
                first
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                first.write_all(b"a").unwrap();
                let mut byte = [0u8; 1];
                first.read_exact(&mut byte).unwrap();
                assert_eq!(&byte, b"a");

                let mut second = TcpStream::connect(relay).unwrap();
                second.write_all(b"b").unwrap();

                drop(first);
                second
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                second.read_exact(&mut byte).unwrap();
                assert_eq!(&byte, b"b");
            }
            {
                let dir = tempfile::tempdir().unwrap();
                let socket = dir.path().join("proxy.sock");
                let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let port = service.local_addr().unwrap().port();
                let _proxy = EgressProxy::start(&socket, EgressPolicy::from_grants(["localhost"])).unwrap();

                let mut all = vec![0x05, 0x01, 0x00, 0x05, 0x01, 0x00, 0x03, 9];
                all.extend_from_slice(b"localhost");
                all.extend_from_slice(&port.to_be_bytes());
                all.extend_from_slice(b"PING");
                let mut client = std::os::unix::net::UnixStream::connect(&socket).unwrap();
                client
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                client.write_all(&all).unwrap();

                let (mut upstream, _) = service.accept().unwrap();
                upstream
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                let mut payload = [0u8; 4];
                upstream.read_exact(&mut payload).unwrap();
                assert_eq!(&payload, b"PING");

                let mut replies = [0u8; 12];
                client.read_exact(&mut replies).unwrap();
                assert_eq!(&replies[..2], &[0x05, 0x00]);
                assert_eq!(replies[3], reply::SUCCESS);
            }
        }

        phase the_loopback_proxy_tunnels_granted_hosts_and_refuses_the_rest;
        {
            let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = service.local_addr().unwrap().port();
            std::thread::spawn(move || {
                while let Ok((mut s, _)) = service.accept() {
                    let _ = s.write_all(b"LOCAL-SERVICE");
                }
            });
            let proxy = LoopbackProxy::start(EgressPolicy::from_grants(["localhost"])).unwrap();
            assert!(proxy.local_addr().ip().is_loopback());
            let connect = |target: &str| {
                let mut client = TcpStream::connect(proxy.local_addr()).unwrap();
                client
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
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
            assert_eq!((proxy.stats().allowed(), proxy.stats().denied()), (1, 1));
        }

        phase the_http_proxy_check_refuses_what_is_not_under_a_declared_domain;
        {
            // The same check SOCKS5 and HTTP CONNECT run (`open_http`), over the wire: a
            // declaration of `*.discord.media` refuses the bare domain and the lookalikes before
            // any name is resolved.
            let proxy = LoopbackProxy::start(EgressPolicy::from_declared(["*.discord.media"]))
                .unwrap();
            let connect = |target: &str| {
                let mut client = TcpStream::connect(proxy.local_addr()).unwrap();
                client
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                client
                    .write_all(format!("CONNECT {target} HTTP/1.1\r\n\r\n").as_bytes())
                    .unwrap();
                let mut answer = Vec::new();
                let _ = client.read_to_end(&mut answer);
                String::from_utf8_lossy(&answer).into_owned()
            };
            for host in ["discord.media", "discord.media.evil", "xdiscord.media"] {
                let refused = connect(&format!("{host}:443"));
                assert!(
                    refused.starts_with("HTTP/1.1 502"),
                    "{host} must not pass the egress check: {refused}"
                );
            }
            assert_eq!(proxy.stats().denied(), 3);
            assert_eq!(proxy.stats().allowed(), 0);
        }

        phase at_the_limit_the_accept_loop_waits_for_a_free_slot;
        {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (queue, queued) = tokio::sync::mpsc::unbounded_channel();
                let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let acceptor = CountedAcceptor {
                    asked: Arc::clone(&asked),
                    queued: tokio::sync::Mutex::new(queued),
                };
                // Two peers in the backlog; the first holds the only slot.
                queue.send(1).unwrap();
                queue.send(2).unwrap();
                let (serving, mut served) = tokio::sync::mpsc::unbounded_channel();
                let release = Arc::new(tokio::sync::Notify::new());
                let held = Arc::clone(&release);
                let (_stop, stop) = oneshot::channel();
                tokio::spawn(accept_bounded(acceptor, 1, stop, move |conn| {
                    let (serving, held) = (serving.clone(), Arc::clone(&held));
                    async move {
                        serving.send(conn).unwrap();
                        if conn == 1 {
                            held.notified().await;
                        }
                    }
                }));
                let first = tilo_test_env::within("the first peer to be served", served.recv());
                assert_eq!(first.await, Some(1));
                // Let the loop run as far as it can on this one thread.
                for _ in 0..16 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    asked.load(Ordering::SeqCst),
                    1,
                    "at the limit the loop leaves the next peer in the backlog"
                );
                assert!(served.try_recv().is_err());
                release.notify_one();
                let second = tilo_test_env::within("the waiting peer to be served", served.recv());
                assert_eq!(second.await, Some(2));
            });
        }
    }

    /// SOCKS5 CONNECT to `localhost:port` through the proxy at `socket`; returns the reply code.
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

    mod calls {
        use super::*;
        use std::os::unix::net::{UnixDatagram, UnixStream};
        use std::time::Instant;

        fn control(path: &Path, app: &str, on: bool) {
            let mut stream = UnixStream::connect(path).expect("control connects");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(format!("calls {app} {}\n", if on { "on" } else { "off" }).as_bytes())
                .unwrap();
            let mut reply = [0u8; 3];
            stream.read_exact(&mut reply).expect("the proxy answered");
            assert_eq!(&reply, b"ok\n");
        }

        fn associate(socket: &Path) -> (UnixStream, u8) {
            let mut stream = UnixStream::connect(socket).expect("proxy connects");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.write_all(&[0x05, 0x01, 0x00]).unwrap();
            let mut greeting = [0u8; 2];
            stream.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting, [0x05, 0x00]);
            // UDP ASSOCIATE, 0.0.0.0:0: the address the client asks from, which it need not know.
            stream
                .write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .unwrap();
            let mut reply = [0u8; 10];
            stream.read_exact(&mut reply).unwrap();
            (stream, reply[1])
        }

        fn frame(dest: SocketAddr, data: &[u8]) -> Vec<u8> {
            let mut frame = vec![0, 0, 0];
            match dest.ip() {
                IpAddr::V4(v4) => {
                    frame.push(0x01);
                    frame.extend_from_slice(&v4.octets());
                }
                IpAddr::V6(v6) => {
                    frame.push(0x04);
                    frame.extend_from_slice(&v6.octets());
                }
            }
            frame.extend_from_slice(&dest.port().to_be_bytes());
            frame.extend_from_slice(data);
            frame
        }

        /// A real UDP echo on loopback: the test's own setup, never a production address.
        fn echo() -> u16 {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let port = socket.local_addr().unwrap().port();
            std::thread::spawn(move || {
                loop {
                    let mut buf = [0u8; 2048];
                    if let Ok((n, from)) = socket.recv_from(&mut buf) {
                        let _ = socket.send_to(&buf[..n], from);
                    }
                }
            });
            port
        }

        fn proxy(dir: &Path, granted: bool) -> (EgressProxy, PathBuf) {
            let socket = dir.join("calls.sock");
            let proxy = EgressProxy::start_for_app_with(
                &socket,
                EgressPolicy::none(),
                "so.tilo.discord",
                granted,
                true,
                8,
            )
            .expect("proxy starts");
            (proxy, socket)
        }

        fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(5) {
                if done() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("timed out waiting for {what}");
        }

        #[test]
        fn a_datagram_outside_a_live_call_is_refused_and_recorded() {
            let dir = tempfile::tempdir().unwrap();
            let (proxy, socket) = proxy(dir.path(), true);
            // No call, so no association at all.
            let (stream, code) = associate(&socket);
            assert_eq!(code, reply::NOT_ALLOWED, "an association needs a live call");
            drop(stream);
            // Even a frame written to the relay's pair carries nothing.
            let client = proxy.udp_client.as_ref().unwrap();
            client
                .send(&frame("127.0.0.1:9".parse().unwrap(), b"nope"))
                .unwrap();
            wait_for("the datagram to be recorded", || {
                proxy.stats().udp_denied() > 0
            });
            assert_eq!(proxy.stats().udp_allowed(), 0);
        }

        #[test]
        fn a_call_carries_a_datagram_to_a_real_echo_and_records_the_flow() {
            let dir = tempfile::tempdir().unwrap();
            let (proxy, socket) = proxy(dir.path(), true);
            let control_file = proxy.control_path().unwrap().to_path_buf();
            control(&control_file, "so.tilo.discord", true);
            let (_stream, code) = associate(&socket);
            assert_eq!(code, reply::SUCCESS);
            let client = proxy.udp_client.as_ref().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            // The new association tells the relay to take its next source.
            let mut reset = [0u8; 1];
            client.recv(&mut reset).expect("the association's reset");
            assert_eq!(reset, [UDP_RELAY_RESET[0]]);
            let port = echo();
            let target: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            client.send(&frame(target, b"voice")).unwrap();
            let mut reply_buf = [0u8; 64];
            let n = client.recv(&mut reply_buf).expect("the echo's reply");
            assert_eq!(&reply_buf[10..n], b"voice", "the reply names its source");
            wait_for("the datagram to be recorded", || {
                proxy.stats().udp_allowed() >= 1
            });
            assert_eq!(proxy.stats().udp_allowed(), 1);
            assert_eq!(
                proxy.stats().udp_flows(),
                1,
                "each destination is one audited flow"
            );
        }

        #[test]
        fn a_call_ending_stops_its_datagrams_at_once() {
            let dir = tempfile::tempdir().unwrap();
            let (proxy, socket) = proxy(dir.path(), true);
            let control_socket = proxy.control_path().unwrap().to_path_buf();
            control(&control_socket, "so.tilo.discord", true);
            let (_stream, code) = associate(&socket);
            assert_eq!(code, reply::SUCCESS);
            let client = proxy.udp_client.as_ref().unwrap();
            client
                .set_read_timeout(Some(Duration::from_millis(300)))
                .unwrap();
            let mut reset = [0u8; 1];
            client.recv(&mut reset).expect("the association's reset");
            let port = echo();
            let target: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            client.send(&frame(target, b"one")).unwrap();
            let mut buf = [0u8; 64];
            assert!(
                client.recv(&mut buf).is_ok(),
                "the first datagram is carried"
            );
            let before = proxy.stats().udp_allowed();
            control(&control_socket, "so.tilo.discord", false);
            client.send(&frame(target, b"two")).unwrap();
            wait_for("the call's end to stop its datagrams", || {
                proxy.stats().udp_denied() > 0 && proxy.stats().udp_allowed() == before
            });
        }

        #[test]
        fn the_association_ends_with_its_control_connection() {
            let dir = tempfile::tempdir().unwrap();
            let (proxy, socket) = proxy(dir.path(), true);
            let control_socket = proxy.control_path().unwrap().to_path_buf();
            control(&control_socket, "so.tilo.discord", true);
            let (stream, code) = associate(&socket);
            assert_eq!(code, reply::SUCCESS);
            let client = proxy.udp_client.as_ref().unwrap();
            let port = echo();
            let target: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            drop(stream);
            wait_for("the association to end", || {
                // The control connection closing ends it; the next datagram finds none.
                client.send(&frame(target, b"late")).ok().is_some()
                    && proxy.stats().udp_denied() > 0
            });
        }

        #[test]
        fn a_share_sized_flow_arrives_whole() {
            let dir = tempfile::tempdir().unwrap();
            let (proxy, socket) = proxy(dir.path(), true);
            let control_socket = proxy.control_path().unwrap().to_path_buf();
            control(&control_socket, "so.tilo.discord", true);
            let (_stream, code) = associate(&socket);
            assert_eq!(code, reply::SUCCESS);
            let client = proxy.udp_client.as_ref().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reset = [0u8; 1];
            client.recv(&mut reset).expect("the association's reset");
            let port = echo();
            let target: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            // A 1080p share's worth: 900 RTP-sized datagrams, several Mbit/s at hundreds a
            // second. The grant and the live call are the bound, so every one comes back whole.
            let payload = [0x5au8; 1200];
            let mut reply_buf = [0u8; 2048];
            for _ in 0..900 {
                client
                    .send(&frame(target, &payload))
                    .expect("a datagram out");
                let n = client
                    .recv(&mut reply_buf)
                    .expect("every datagram comes back");
                assert_eq!(&reply_buf[10..n], &payload[..], "the share's bytes, whole");
            }
            assert_eq!(proxy.stats().udp_allowed(), 900);
            assert_eq!(proxy.stats().udp_denied(), 0, "nothing was dropped");
        }

        #[test]
        fn an_app_without_the_grant_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let (proxy, socket) = proxy(dir.path(), false);
            assert!(proxy.control_path().is_none(), "no calls, no lane");
            let (_stream, code) = associate(&socket);
            assert_eq!(code, reply::NOT_ALLOWED);
            assert!(proxy.stats().udp_denied() > 0);
        }

        #[test]
        fn the_relay_carries_only_the_client_that_sent_first() {
            use std::os::fd::IntoRawFd as _;
            let (proxy_end, relay_end) = UnixDatagram::pair().expect("a datagram pair");
            let port = {
                let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                socket.local_addr().unwrap().port()
            };
            let fd = relay_end.into_raw_fd();
            std::thread::spawn(move || {
                let _ = run_udp_relay(fd, port);
            });
            proxy_end
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let first = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let foreign = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let relay: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            wait_for("the relay to bind", || {
                first.send_to(b"first", relay).is_ok() && proxy_end.recv(&mut [0u8; 16]).is_ok()
            });
            // The first source is the client; a second loopback source is not relayed. Drain the
            // copies the wait may have sent, so the next read is the foreign one or nothing.
            proxy_end
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let mut buf = [0u8; 16];
            while proxy_end.recv(&mut buf).is_ok() {}
            foreign.send_to(b"foreign", relay).unwrap();
            let got = proxy_end.recv(&mut buf);
            assert!(
                got.is_err(),
                "a datagram from a foreign source must be dropped, got {:?}",
                got.map(|n| &buf[..n])
            );
        }
    }
}
