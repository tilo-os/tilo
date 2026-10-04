//! What a build sandbox may reach.
//!
//! A build downloads packages and nothing else, so by default it reaches the
//! runtime's package repositories and nothing else ([`Network::Gated`]): the
//! sandbox gets its own network namespace, and [`tilo_egress`] puts a DNS
//! stub and a packet filter in it that admit only those hosts. A scriptlet
//! that tries to phone anywhere else gets no answer for the name and no
//! route to an address.
//!
//! The hosts are what the runtime's `etc/yum.repos.d` names. A `metalink`
//! names a service that answers with mirrors on other hosts, so each one is
//! fetched first — with the runtime's own `curl`, in a sandbox on the
//! caller's network that holds nothing but the runtime — and the mirrors it
//! lists are allowed too.

use std::path::Path;
use std::time::Duration;

use tilo_egress::{EgressOptions, EgressPolicy, RepoHosts};

use crate::sandbox::{BuildSandbox, SandboxError};

/// How a build sandbox is networked.
#[derive(Debug, Clone)]
pub enum Network {
    /// Its own namespace, reaching only `policy`'s hosts. Setting it up
    /// fails closed: no egress, no build.
    Gated {
        policy: EgressPolicy,
        options: EgressOptions,
    },
    /// Its own namespace with nothing in it (a build that downloads
    /// nothing, such as a fixture).
    Offline,
    /// The caller's network, unfiltered: for a CI catalog build of a fixed
    /// list on a builder that has no `/dev/net/tun` for the gate.
    Host,
}

/// Which [`Network`] a builder gives its sandboxes; [`NetworkChoice::Gated`]
/// works out the hosts per runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NetworkChoice {
    #[default]
    Gated,
    Offline,
    Host,
}

/// How long fetching one metalink may take.
const MIRROR_LIST_TIMEOUT: Duration = Duration::from_secs(60);

/// The policy for builds on the runtime at `tree`: its repositories' hosts
/// and the mirrors their metalinks list. `scratch` is for the fetch's
/// sandbox and is left empty.
///
/// # Errors
///
/// The repositories can't be read, or a metalink can't be fetched (the
/// build would not find its mirrors).
pub fn repo_policy(tree: &Path, scratch: &Path) -> Result<EgressPolicy, SandboxError> {
    let mut repos =
        RepoHosts::read(tree, std::env::consts::ARCH).map_err(|source| SandboxError::Io {
            path: tree.join("etc/yum.repos.d"),
            source,
        })?;
    if !repos.mirror_lists.is_empty() {
        let fetcher = BuildSandbox::create(tree, scratch)?.with_network(Network::Host);
        for url in repos.mirror_lists.clone() {
            let ran = fetcher.run(
                &["curl", "-fsSL", "--max-time", "50", "--", &url],
                MIRROR_LIST_TIMEOUT,
            )?;
            if !ran.success {
                return Err(SandboxError::Egress(format!(
                    "fetching the mirror list {url}: {}",
                    ran.stderr.trim()
                )));
            }
            repos.add_mirrors(&ran.stdout);
        }
        let _ = fetcher.discard();
    }
    tracing::debug!(hosts = ?repos.hosts, "build egress");
    Ok(repos.policy())
}

/// [`Network::Gated`] with this machine's resolvers and `pasta`.
pub fn gated(policy: EgressPolicy) -> Network {
    Network::Gated {
        policy,
        options: EgressOptions::from_host(Path::new("/")),
    }
}
