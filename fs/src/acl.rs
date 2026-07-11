//! Per-identity workspace ACL — the filesystem enforcement point.
//!
//! The coherence architecture (`docs/coherence-architecture.md` §2)
//! makes the FUSE driver *the* enforcement point for file access:
//!
//! > Every open/read/write is checked against the workspace ACL keyed on
//! > *identity*, not archaic mode bits. Unix mode bits are a
//! > **synthesized compatibility veneer** … the app runs as the user,
//! > the *fs* enforces per-identity policy.
//!
//! This module is that ACL. It maps an **identity** (a 32-byte public
//! key — the same key space as [`qos_user_identity`] device/user keys)
//! to the paths it may read and/or write. [`QuickFS`](crate::QuickFS)
//! is bound to one identity per mount (the session's identity) and, when
//! an ACL is attached, checks every `open`/`read`/`write` against it.
//!
//! **Fail closed.** An identity has an access on a path only if some
//! entry explicitly grants it. No matching entry ⇒ denied. A mount with
//! *no* ACL attached keeps the pre-existing behaviour (the Unix veneer
//! only) so unshared single-user mounts are unaffected — the ACL is the
//! opt-in enforcement layer, and once attached it is authoritative.

use std::collections::HashSet;

/// A qOS identity — the 32-byte Ed25519 public key that anchors a
/// person (or one of their devices) across the system.
pub type IdentityKey = [u8; 32];

/// The kind of access being attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// `open` for reading, or a `read`.
    Read,
    /// `open` for writing, `write`, `create`, `truncate`, unlink, …
    Write,
}

/// One grant: an identity may read and/or write a path subtree.
///
/// `path_prefix` matches the path itself and everything beneath it:
/// `"/"` matches everything; `"/docs"` matches `/docs` and `/docs/**`
/// but not `/docsxyz`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AclEntry {
    /// Identity this grant is for.
    pub identity: IdentityKey,
    /// Path subtree the grant covers.
    pub path_prefix: String,
    /// May read within the subtree.
    pub read: bool,
    /// May write within the subtree.
    pub write: bool,
}

/// A workspace's per-identity access-control list.
///
/// A flat list of [`AclEntry`]s evaluated with union semantics: an
/// identity holds an access on a path if **any** matching entry grants
/// it. There is no explicit-deny entry in v1 — absence *is* denial
/// (fail-closed) — which keeps the model small and total.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceAcl {
    entries: Vec<AclEntry>,
}

impl WorkspaceAcl {
    /// An empty ACL. Fail-closed: it grants nothing to anyone.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a raw entry.
    pub fn push(&mut self, entry: AclEntry) -> &mut Self {
        self.entries.push(entry);
        self
    }

    /// Grant `identity` read (and optionally write) on `path_prefix`.
    pub fn grant(
        &mut self,
        identity: IdentityKey,
        path_prefix: impl Into<String>,
        read: bool,
        write: bool,
    ) -> &mut Self {
        self.entries.push(AclEntry {
            identity,
            path_prefix: path_prefix.into(),
            read,
            write,
        });
        self
    }

    /// Convenience: grant read-only on a subtree.
    pub fn grant_read(&mut self, identity: IdentityKey, path_prefix: impl Into<String>) -> &mut Self {
        self.grant(identity, path_prefix, true, false)
    }

    /// Convenience: grant read+write on a subtree.
    pub fn grant_read_write(
        &mut self,
        identity: IdentityKey,
        path_prefix: impl Into<String>,
    ) -> &mut Self {
        self.grant(identity, path_prefix, true, true)
    }

    /// The set of identities that appear in the ACL (any grant).
    pub fn identities(&self) -> HashSet<IdentityKey> {
        self.entries.iter().map(|e| e.identity).collect()
    }

    /// Does `identity` hold `access` on `path`?
    ///
    /// Fail-closed: returns `true` only if an entry for this identity
    /// whose prefix covers `path` grants the requested access.
    pub fn permits(&self, identity: &IdentityKey, path: &str, access: Access) -> bool {
        self.entries.iter().any(|e| {
            &e.identity == identity
                && prefix_covers(&e.path_prefix, path)
                && match access {
                    Access::Read => e.read,
                    Access::Write => e.write,
                }
        })
    }
}

/// Does `prefix` cover `path`? `"/"` covers everything; otherwise `path`
/// must equal `prefix` or start with `prefix` + `"/"` (component
/// boundary, so `/doc` does not cover `/document`).
fn prefix_covers(prefix: &str, path: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    if path == prefix {
        return true;
    }
    let mut with_sep = String::with_capacity(prefix.len() + 1);
    with_sep.push_str(prefix);
    with_sep.push('/');
    path.starts_with(&with_sep)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: IdentityKey = [1u8; 32];
    const BOB: IdentityKey = [2u8; 32];

    #[test]
    fn empty_acl_denies_everyone() {
        let acl = WorkspaceAcl::new();
        assert!(!acl.permits(&ALICE, "/anything", Access::Read));
        assert!(!acl.permits(&ALICE, "/", Access::Write));
    }

    #[test]
    fn read_grant_allows_read_not_write() {
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(ALICE, "/docs");
        assert!(acl.permits(&ALICE, "/docs", Access::Read));
        assert!(acl.permits(&ALICE, "/docs/report.md", Access::Read));
        assert!(!acl.permits(&ALICE, "/docs", Access::Write));
        // Bob has no grant at all.
        assert!(!acl.permits(&BOB, "/docs/report.md", Access::Read));
    }

    #[test]
    fn prefix_respects_component_boundary() {
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(ALICE, "/doc");
        assert!(acl.permits(&ALICE, "/doc/a", Access::Read));
        assert!(acl.permits(&ALICE, "/doc", Access::Read));
        // Not a subtree of "/doc" — must not leak.
        assert!(!acl.permits(&ALICE, "/document", Access::Read));
    }

    #[test]
    fn root_prefix_covers_all() {
        let mut acl = WorkspaceAcl::new();
        acl.grant_read_write(ALICE, "/");
        assert!(acl.permits(&ALICE, "/x/y/z", Access::Read));
        assert!(acl.permits(&ALICE, "/x/y/z", Access::Write));
    }
}
