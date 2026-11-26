//! Evaluating an environment's grants.
//!
//! There is no `ush` permission type: a grant is a `collab_types::caps::CapOverride` in
//! the environment record, evaluated by the same `decide_capability` the compositor's
//! permissions panel and the FUSE mount use. `perm trust-dir ~/src` mints exactly what the
//! compositor writes for "Always allow" on a filesystem prompt (see
//! [`tilo_environment::dir_grant`]).
//!
//! Deny by default: only a `Grant` nominates a path, only one the runtime is willing to
//! bind ([`tilo_environment::is_grantable`]), and only while `decide_capability` still
//! allows it — a `Restrict` narrows it and an expired grant is gone.

use std::path::{Path, PathBuf};

use collab_types::caps::{
    Access, CapDecision, CapOverride, Effect, ResourceRef, Scope, decide_app,
};
use tilo_action_policy::Grantee;
use tilo_environment::{RUNTIME_APP_ID, is_grantable};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub capabilities: Vec<CapOverride>,
}

impl Policy {
    pub fn new(capabilities: Vec<CapOverride>) -> Self {
        Self { capabilities }
    }

    pub fn trusted_dirs(&self, now: Option<u64>) -> Vec<PathBuf> {
        self.paths_where(Access::Write, now)
    }

    /// Every directory the guest may read but not write.
    pub fn readable_dirs(&self, now: Option<u64>) -> Vec<PathBuf> {
        let writable = self.trusted_dirs(now);
        self.paths_where(Access::Read, now)
            .into_iter()
            .filter(|p| !writable.contains(p))
            .collect()
    }

    fn paths_where(&self, verb: Access, now: Option<u64>) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for cap in &self.capabilities {
            // Only a Grant nominates a path; a Restrict can take one away (through
            // `decide_app` below) but never introduce one.
            if cap.effect != Effect::Grant || !cap.names_app(RUNTIME_APP_ID) {
                continue;
            }
            let Scope::FsPath(raw) = &cap.scope else {
                continue;
            };
            if !is_grantable(Path::new(raw)) {
                tracing::warn!(path = %raw, "ignoring a grant the runtime will not bind");
                continue;
            }
            let resource = ResourceRef::FsPath(raw.clone());
            if decide_app(
                RUNTIME_APP_ID,
                &resource,
                verb,
                &self.capabilities,
                now.map(|now| now.saturating_mul(1000)),
            ) != CapDecision::Allow
            {
                continue;
            }
            let path = PathBuf::from(raw);
            if !out.contains(&path) {
                out.push(path);
            }
        }
        out.sort();
        out
    }

    /// The hosts the guest may reach, as their grants spell them (`*`,
    /// `example.com`, `.example.com`), as of `now`. The same evaluation the
    /// launcher gives an app's `NetHost` capabilities: a grant names a host,
    /// and `decide_capability` must still allow it.
    pub fn net_hosts(&self, now: Option<u64>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for cap in &self.capabilities {
            if cap.effect != Effect::Grant || !cap.names_app(RUNTIME_APP_ID) {
                continue;
            }
            let Scope::NetHost(host) = &cap.scope else {
                continue;
            };
            // A stored grant that is not a host in the one grammar (an IP
            // literal, written before addresses were refused) covers nothing.
            if let Err(reason) = collab_types::caps::NetHostPattern::parse(host) {
                tracing::warn!(%reason, "ignoring a network grant that is not a host");
                continue;
            }
            let resource = ResourceRef::NetHost(host.clone());
            if decide_app(
                RUNTIME_APP_ID,
                &resource,
                Access::Read,
                &self.capabilities,
                now.map(|now| now.saturating_mul(1000)),
            ) != CapDecision::Allow
            {
                continue;
            }
            if !out.contains(host) {
                out.push(host.clone());
            }
        }
        out.sort();
        out
    }

    pub fn egress(&self, now: Option<u64>) -> tilo_egress::EgressPolicy {
        let hosts = self.net_hosts(now);
        tilo_egress::EgressPolicy::from_grants(hosts.iter().map(String::as_str))
    }

    pub fn permits(&self, path: &Path, verb: Access, now: Option<u64>) -> bool {
        tilo_environment::files::permits(&self.capabilities, path, verb, now)
    }

    /// A policy that answers yes to every path, for the host's own login
    /// shell: no environment record exists there, so there is no grant to
    /// check and the person's own permissions are the only bound. It is never
    /// built for a guest, whose record always says what it may reach.
    #[must_use]
    pub fn unbounded() -> Self {
        Self {
            capabilities: vec![
                CapOverride::new(
                    Grantee::person("local"),
                    Scope::FsPath("/".to_string()),
                    Effect::Grant,
                    Access::Write,
                )
                .through(RUNTIME_APP_ID),
            ],
        }
    }

    /// Whether `path` itself or the directory it would be written into is
    /// granted for `verb`.
    ///
    /// A grant names a directory, and the files in it are created later, so a
    /// question about a file that does not exist yet is a question about its
    /// parent. Answering only the path would refuse every `mkdir ~/src/new`
    /// and every file a command writes into a granted directory.
    #[must_use]
    pub fn permits_in(&self, path: &Path, verb: Access, now: Option<u64>) -> bool {
        tilo_environment::files::permits_in(&self.capabilities, path, verb, now)
    }
}
