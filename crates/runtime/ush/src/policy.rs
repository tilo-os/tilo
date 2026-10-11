//! Evaluating an environment's grants.
//!
//! # There is no `ush` permission type
//!
//! A grant is a `collab_types::caps::CapOverride` in the environment record,
//! evaluated by the same `decide_capability` the compositor's permissions
//! panel and `tilo-launcher`/the FUSE mount use. `perm trust-dir ~/src` mints
//! exactly:
//!
//! ```text
//! CapOverride { subject: Principal::App("so.tilo.ush"),
//!               scope:   Scope::FsPath("/var/home/you/src"),
//!               effect:  Effect::Grant,
//!               verb:    Access::Write }
//! ```
//!
//! byte-for-byte what the compositor writes for "Always allow" on a
//! filesystem prompt (see [`tilo_environment::dir_grant`]).
//!
//! # Deny by default
//!
//! Only a `Grant` nominates a path, only one the runtime is willing to bind
//! ([`tilo_environment::is_grantable`]), and only while `decide_capability`
//! still allows it — a `Restrict` narrows it, an expired grant is gone.

use std::path::{Path, PathBuf};

use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Effect, ResourceRef, Scope, decide_capability,
};
use tilo_environment::{RUNTIME_APP_ID, is_grantable};

/// An environment's grants, ready to be asked about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub capabilities: Vec<CapOverride>,
}

impl Policy {
    pub fn new(capabilities: Vec<CapOverride>) -> Self {
        Self { capabilities }
    }

    /// The actor every question is asked about. `role: None` is load-bearing:
    /// `collab_types::role_base_applies` returns false for `FsPath`, `Device`
    /// and `NetHost`, so no role — not even Owner — reaches a path; every bind
    /// exists because a capability says so.
    pub fn actor() -> Actor {
        Actor::app(RUNTIME_APP_ID)
    }

    /// Every directory the guest may write, as of `now`.
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
        let actor = Self::actor();
        let mut out: Vec<PathBuf> = Vec::new();
        for cap in &self.capabilities {
            // Only a Grant nominates a path; a Restrict can take one away (through
            // `decide_capability` below) but never introduce one.
            if cap.effect != Effect::Grant || !cap.subject.matches_actor(&actor) {
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
            if decide_capability(&actor, &resource, verb, &self.capabilities, now)
                != CapDecision::Allow
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
        let actor = Self::actor();
        let mut out: Vec<String> = Vec::new();
        for cap in &self.capabilities {
            if cap.effect != Effect::Grant || !cap.subject.matches_actor(&actor) {
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
            if decide_capability(&actor, &resource, Access::Read, &self.capabilities, now)
                != CapDecision::Allow
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

    /// What the guest's network allows, as of `now`.
    pub fn egress(&self, now: Option<u64>) -> tilo_egress::EgressPolicy {
        let hosts = self.net_hosts(now);
        tilo_egress::EgressPolicy::from_grants(hosts.iter().map(String::as_str))
    }

    /// Is `path` reachable at `verb`? The question `scan` and the broker ask.
    pub fn permits(&self, path: &Path, verb: Access, now: Option<u64>) -> bool {
        let actor = Self::actor();
        let resource = ResourceRef::FsPath(path.to_string_lossy().into_owned());
        decide_capability(&actor, &resource, verb, &self.capabilities, now) == CapDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{Expiry, Principal, Subject};
    use tilo_environment::dir_grant;

    fn granted(path: &str, write: bool) -> Policy {
        Policy::new(vec![dir_grant(Path::new(path), write).unwrap()])
    }

    #[test]
    fn a_grant_is_recursive_and_bounded_by_components() {
        let policy = granted("/var/home/you/src", true);
        assert_eq!(
            policy.trusted_dirs(Some(1_000)),
            vec![PathBuf::from("/var/home/you/src")]
        );
        assert!(policy.permits(
            Path::new("/var/home/you/src/deep/file.rs"),
            Access::Write,
            Some(1_000)
        ));
        assert!(!policy.permits(Path::new("/var/home/you/.ssh"), Access::Read, Some(1_000)));
        assert!(
            !policy.permits(
                Path::new("/var/home/you/srcextra"),
                Access::Write,
                Some(1_000)
            ),
            "a component-boundary check must not be a prefix check"
        );

        let read_only = granted("/var/home/you/notes", false);
        assert!(read_only.trusted_dirs(Some(1_000)).is_empty());
        assert_eq!(
            read_only.readable_dirs(Some(1_000)),
            vec![PathBuf::from("/var/home/you/notes")]
        );
    }

    #[test]
    fn restrictions_and_expiry_narrow_a_grant() {
        // The one that makes this not a second permission system: a `Restrict`
        // written by the compositor removes the writable bind here.
        let mut policy = granted("/var/home/you/src", true);
        policy.capabilities.push(CapOverride::new(
            Subject::Principal(Principal::App(RUNTIME_APP_ID.to_string())),
            Scope::FsPath("/var/home/you/src".to_string()),
            Effect::Restrict,
            Access::Read,
        ));
        assert!(policy.trusted_dirs(Some(1_000)).is_empty());
        assert_eq!(
            policy.readable_dirs(Some(1_000)),
            vec![PathBuf::from("/var/home/you/src")]
        );

        let mut expiring = granted("/var/home/you/src", true);
        expiring.capabilities[0].expiry = Expiry::AtUnix(500);
        assert_eq!(expiring.trusted_dirs(Some(499)).len(), 1);
        assert!(expiring.trusted_dirs(Some(501)).is_empty());
    }

    #[test]
    fn host_grants_become_the_egress_policy_and_expire() {
        let mut policy = Policy::new(vec![
            tilo_environment::host_grant("github.com").unwrap(),
            tilo_environment::host_grant("*.crates.io").unwrap(),
            dir_grant(Path::new("/var/home/you/src"), true).unwrap(),
        ]);
        assert_eq!(policy.net_hosts(Some(1_000)), [".crates.io", "github.com"]);
        let egress = policy.egress(Some(1_000));
        assert!(egress.allows("static.crates.io").is_some());
        assert!(egress.allows("github.com").is_some());
        assert!(egress.allows("example.com").is_none());

        policy.capabilities[0].expiry = Expiry::AtUnix(500);
        assert_eq!(policy.net_hosts(Some(501)), [".crates.io"]);

        // A host grant for another app grants this guest nothing.
        let mut other = tilo_environment::host_grant("example.com").unwrap();
        other.subject = Subject::Principal(Principal::App("so.tilo.other".into()));
        assert!(Policy::new(vec![other]).net_hosts(Some(1)).is_empty());

        let any = Policy::new(vec![tilo_environment::host_grant("*").unwrap()]);
        assert_eq!(any.egress(Some(1)), tilo_egress::EgressPolicy::Any);
        assert_eq!(
            Policy::default().egress(Some(1)),
            tilo_egress::EgressPolicy::none()
        );
    }

    #[test]
    fn a_grant_the_runtime_would_not_bind_binds_nothing() {
        // Written by something other than `grant-dir`, which refuses it.
        let policy = Policy::new(vec![CapOverride::grant(
            Subject::Principal(Principal::App(RUNTIME_APP_ID.to_string())),
            Scope::FsPath("/usr".to_string()),
            Access::Write,
        )]);
        assert!(policy.trusted_dirs(Some(1_000)).is_empty());
    }
}
