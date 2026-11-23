//! DNS-allowlisted egress for a sandbox in a network namespace of its own: the
//! sandbox's only resolver is a stub that answers just the names the policy
//! allows, and the packet filter admits only the addresses those answers
//! carried, for as long as their TTL.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod config;

pub mod allowed;
pub mod dns;
#[cfg(target_os = "linux")]
pub mod netns;
#[cfg(target_os = "linux")]
pub mod nft;
#[cfg(target_os = "linux")]
pub mod pasta;
pub mod policy;
pub mod repos;
#[cfg(target_os = "linux")]
pub mod stub;

use std::net::{IpAddr, SocketAddr};
#[cfg(target_os = "linux")]
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex, RwLock};

#[cfg(target_os = "linux")]
pub use netns::Netns;
pub use policy::{Allowed, EgressPolicy, HostPattern};
pub use repos::{RepoHosts, metalink_mirror_hosts};

#[cfg(target_os = "linux")]
use crate::allowed::AllowedAddrs;
#[cfg(target_os = "linux")]
use crate::nft::Netfilter;
#[cfg(target_os = "linux")]
use crate::pasta::Pasta;
#[cfg(target_os = "linux")]
use crate::stub::{Filter, Shared, Stub};

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

#[derive(Debug, Clone)]
pub struct EgressOptions {
    pub upstream: Vec<SocketAddr>,
    pub pasta: PathBuf,
    /// Map this address in the sandbox to the host's loopback. `None` in
    /// production: a sandbox must not reach services listening only on the host.
    pub map_host_loopback: Option<IpAddr>,
    /// Act as if the kernel refused the packet filter, to prove the fail-closed
    /// path on a machine whose kernel would accept it.
    pub refuse_netfilter: bool,
}

impl EgressOptions {
    pub fn from_host(host_root: &Path) -> Self {
        let conf = std::fs::read_to_string(host_root.join("etc/resolv.conf")).unwrap_or_default();
        Self {
            upstream: config::nameservers(&conf),
            pasta: config::find_pasta(),
            map_host_loopback: None,
            refuse_netfilter: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enforcement {
    Unrestricted,
    /// The filter is installed: only answered addresses of these hosts.
    Filtered(Vec<String>),
    Offline,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct Egress {
    shared: Arc<Shared>,
    pasta: Option<Pasta>,
    _stub: Stub,
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
impl Egress {
    pub fn resolv_conf(&self) -> &'static str {
        RESOLV_CONF
    }

    pub fn policy(&self) -> EgressPolicy {
        self.shared
            .policy
            .read()
            .map_or_else(|_| EgressPolicy::none(), |p| p.clone())
    }

    pub fn pasta_pid(&self) -> Option<u32> {
        self.pasta.as_ref().and_then(Pasta::pid)
    }

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

    /// Change the policy of a running sandbox. A narrower one empties the allowed
    /// sets, so nothing answered under the old policy stays reachable.
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
