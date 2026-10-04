//! DNS-allowlisted egress for a sandbox in its own network namespace.
//!
//! A sandbox that may reach `github.com` and nothing else cannot be given a
//! proxy: command-line tools ignore proxies. So the decision is made where
//! no tool can skip it — in the kernel — keyed by the one thing every tool
//! does first, resolving the name:
//!
//! ```text
//!  sandbox netns (own user namespace)              supervisor
//!  ┌─────────────────────────────────┐
//!  │ resolv.conf → 127.0.0.1:53 ─────┼──► DNS stub ── granted name? ──► host resolver
//!  │                                 │       │  no: NXDOMAIN
//!  │ nftables output chain           │       └─ every A/AAAA answered ──► allowed set
//!  │   lo, established: accept       │                                   (record TTL,
//!  │   dst in allowed set: accept ◄──┼───────────────────────────────────  60 s..4 h)
//!  │   anything else: reject         │
//!  │ tap ◄───────────────────────────┼──► pasta ──► the host's network
//!  └─────────────────────────────────┘
//! ```
//!
//! An address literal is not in the set, so it is refused; so is DNS to any
//! server but the stub. A `*` grant ([`EgressPolicy::Any`]) installs no
//! filter and forwards every name. No grants installs the filter with empty
//! sets, and the stub refuses every name.
//!
//! **Fail closed.** When the policy needs the filter and the kernel will
//! not take it, [`spawn_egress`] fails and the caller does not start the
//! sandbox. `pasta` is only attached once the filter is in place; before
//! that the namespace has no route out at all.
//!
//! The filter matches addresses, so a granted host shares its address with
//! whatever else is served from it (a CDN). Names are what the user grants;
//! addresses are the closest the kernel can enforce.
//!
//! # Using it
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use tilo_egress::{EgressOptions, EgressPolicy, Netns, spawn_egress};
//! let netns = Netns::create()?;
//! let policy = EgressPolicy::from_grants(["github.com", ".crates.io"]);
//! let egress = spawn_egress(&netns, policy, &EgressOptions::from_host(std::path::Path::new("/")))?;
//! // Bind `egress.resolv_conf()` at the sandbox's /etc/resolv.conf, then:
//! let mut cmd = std::process::Command::new("bwrap");
//! netns.enter(&mut cmd);
//! # Ok(()) }
//! ```

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod allowed;
pub mod dns;
pub mod netns;
pub mod nft;
pub mod pasta;
pub mod policy;
pub mod repos;
pub mod stub;

use std::net::{IpAddr, SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

pub use netns::Netns;
pub use policy::{Allowed, EgressPolicy, HostPattern};
pub use repos::{RepoHosts, metalink_mirror_hosts};

use crate::allowed::AllowedAddrs;
use crate::nft::Netfilter;
use crate::pasta::Pasta;
use crate::stub::{Filter, Shared, Stub};

/// The sandbox's `/etc/resolv.conf`: the stub, and nothing else.
pub const RESOLV_CONF: &str = "# The sandbox's resolver answers only the hosts it was granted.\n\
                               nameserver 127.0.0.1\n\
                               options edns0\n";

/// Why egress could not be set up. Every variant means the sandbox must not
/// start: none of them leaves the network open.
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    /// The namespace's sockets could not be opened.
    #[error("the sandbox's network namespace could not be prepared: {0}")]
    Namespace(#[source] std::io::Error),
    /// The policy needs the packet filter and the kernel refused it.
    #[error(
        "the network filter could not be installed ({0}). Hosts are granted, and without \
         the filter they could not be enforced, so the sandbox does not start. The kernel \
         needs nf_tables (CONFIG_NF_TABLES, nft_ct, nft_reject_inet)"
    )]
    Netfilter(String),
    /// The policy reaches something and `pasta` would not attach.
    #[error("the sandbox's network could not be connected: {0}")]
    Pasta(String),
    /// A thread or socket for the DNS stub.
    #[error("the sandbox's DNS stub could not start: {0}")]
    Stub(#[source] std::io::Error),
}

/// How egress is wired on this machine.
#[derive(Debug, Clone)]
pub struct EgressOptions {
    /// The resolvers the stub forwards granted names to.
    pub upstream: Vec<SocketAddr>,
    /// The `pasta` binary.
    pub pasta: PathBuf,
    /// Map this address in the sandbox to the host's loopback. `None` in
    /// production — a sandbox must not reach services listening only on
    /// the machine. Tests set it to reach a server they run.
    pub map_host_loopback: Option<IpAddr>,
    /// Act as if the kernel refused the packet filter, to prove the
    /// fail-closed path on a machine whose kernel would accept it.
    pub refuse_netfilter: bool,
}

impl EgressOptions {
    /// The machine's resolvers (from `<host_root>/etc/resolv.conf`) and the
    /// `pasta` on `PATH`.
    pub fn from_host(host_root: &Path) -> Self {
        let conf = std::fs::read_to_string(host_root.join("etc/resolv.conf")).unwrap_or_default();
        Self {
            upstream: stub::nameservers(&conf),
            pasta: pasta::find(),
            map_host_loopback: None,
            refuse_netfilter: false,
        }
    }
}

/// What a running egress enforces, for `guard status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enforcement {
    /// No filter: every name resolves, every address is reachable.
    Unrestricted,
    /// The filter is installed: only answered addresses of these hosts.
    Filtered(Vec<String>),
    /// No route out at all (no grants, and no filter to add one later).
    Offline,
}

/// Egress for one sandbox: the stub, the filter and `pasta`. Dropping it
/// stops all three; the namespace itself belongs to the [`Netns`].
#[derive(Debug)]
pub struct Egress {
    shared: Arc<Shared>,
    pasta: Option<Pasta>,
    _stub: Stub,
}

/// Start egress for `netns` under `policy`. The filter is installed (when
/// the policy needs one) before `pasta` attaches, and the stub is serving
/// before this returns, so the sandbox can start right after.
///
/// # Errors
/// See [`EgressError`]; every error means the sandbox must not start.
pub fn spawn_egress(
    netns: &Netns,
    policy: EgressPolicy,
    options: &EgressOptions,
) -> Result<Egress, EgressError> {
    let inside = netns.open_inside().map_err(EgressError::Namespace)?;
    let netfilter = if options.refuse_netfilter {
        Err("refused for this test".to_owned())
    } else {
        inside
            .netfilter
            .and_then(Netfilter::new)
            .map_err(|e| e.to_string())
    };
    let netfilter = netfilter.and_then(|mut n| {
        if matches!(policy, EgressPolicy::Any) {
            return Ok(n);
        }
        n.install().map(|()| n).map_err(|e| e.to_string())
    });
    let needs_route = policy.reaches_anything();
    if needs_route
        && !matches!(policy, EgressPolicy::Any)
        && let Err(reason) = &netfilter
    {
        return Err(EgressError::Netfilter(reason.clone()));
    }
    if !needs_route && let Err(reason) = &netfilter {
        tracing::info!(%reason, "egress: no packet filter; the sandbox stays offline");
    }
    // Attach the route out only behind an installed filter (or for `*`).
    let filtered = netfilter.as_ref().is_ok_and(Netfilter::installed);
    let pasta = if matches!(policy, EgressPolicy::Any) || filtered {
        match Pasta::start(&options.pasta, netns, options.map_host_loopback) {
            Ok(p) => Some(p),
            Err(reason) if needs_route => return Err(EgressError::Pasta(reason)),
            Err(reason) => {
                tracing::info!(%reason, "egress: no route out; the sandbox stays offline");
                None
            }
        }
    } else {
        None
    };
    let shared = Arc::new(Shared {
        policy: RwLock::new(policy),
        filter: Mutex::new(Filter {
            netfilter: netfilter.ok(),
            allowed: AllowedAddrs::default(),
        }),
        upstream: options.upstream.clone(),
    });
    let udp = UdpSocket::from(inside.dns_udp);
    let tcp = TcpListener::from(inside.dns_tcp);
    let stub = Stub::start(udp, tcp, &shared).map_err(EgressError::Stub)?;
    Ok(Egress {
        shared,
        pasta,
        _stub: stub,
    })
}

impl Egress {
    /// The sandbox's `/etc/resolv.conf`.
    pub fn resolv_conf(&self) -> &'static str {
        RESOLV_CONF
    }

    /// The policy in force.
    pub fn policy(&self) -> EgressPolicy {
        self.shared
            .policy
            .read()
            .map_or_else(|_| EgressPolicy::none(), |p| p.clone())
    }

    /// The process id of `pasta`, when the sandbox has a route out.
    pub fn pasta_pid(&self) -> Option<u32> {
        self.pasta.as_ref().and_then(Pasta::pid)
    }

    /// What is enforced now.
    pub fn enforcement(&self) -> Enforcement {
        let filtered = self
            .shared
            .filter
            .lock()
            .is_ok_and(|f| f.netfilter.as_ref().is_some_and(Netfilter::installed));
        match self.policy() {
            EgressPolicy::Any if self.pasta.is_some() && !filtered => Enforcement::Unrestricted,
            policy if filtered && self.pasta.is_some() => Enforcement::Filtered(policy.describe()),
            _ => Enforcement::Offline,
        }
    }

    /// Change the policy of a running sandbox. A narrower policy empties
    /// the allowed sets, so nothing answered under the old one stays
    /// reachable; connections already open are left alone.
    ///
    /// # Errors
    /// Returns why the change needs a new sandbox (a route out that was
    /// never attached, a filter the kernel refuses), in words for the user.
    /// The old policy stays in force.
    pub fn update_policy(&self, policy: EgressPolicy) -> Result<(), String> {
        let old = self.policy();
        if old == policy {
            return Ok(());
        }
        let mut filter = self
            .shared
            .filter
            .lock()
            .map_err(|_| "the filter lock is poisoned".to_owned())?;
        let needs_route = policy.reaches_anything();
        if needs_route && self.pasta.is_none() {
            return Err("this session was started without a network; it needs a restart".into());
        }
        match &policy {
            EgressPolicy::Any => {
                if let Some(n) = filter.netfilter.as_mut() {
                    n.remove()
                        .map_err(|e| format!("removing the filter: {e}"))?;
                }
                filter.allowed.clear();
            }
            EgressPolicy::Hosts(_) => {
                let n = filter
                    .netfilter
                    .as_mut()
                    .ok_or("this session has no packet filter; it needs a restart")?;
                let fresh = !n.installed();
                n.install()
                    .map_err(|e| format!("installing the filter: {e}"))?;
                // Narrower: nothing answered under the old policy may stay
                // reachable. Wider: what was admitted stays admitted.
                if !fresh && !old.within(&policy) {
                    n.flush()
                        .map_err(|e| format!("emptying the allowed sets: {e}"))?;
                    filter.allowed.clear();
                }
            }
        }
        // Written under the filter lock: the stub admits only while holding
        // it, and checks the policy again there, so an answer to a name the
        // old policy allowed cannot land in the sets after they were emptied.
        if let Ok(mut p) = self.shared.policy.write() {
            *p = policy;
        }
        drop(filter);
        Ok(())
    }
}
