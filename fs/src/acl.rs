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
//!
//! # Capabilities are the source of truth
//!
//! The system's one permission vocabulary is
//! [`collab_types::CapOverride`] — `(subject × scope × verb)` plus an
//! expiry. [`WorkspaceAcl::from_capabilities`] binds a capability list
//! to a mount, and from then on every `permits` call for that identity
//! is answered by [`collab_types::decide_capability`] itself. Nothing is
//! re-derived here: path containment is [`collab_types::path_covers`],
//! expiry is [`collab_types::Expiry`], and the deny-bias (a `Restrict`
//! beats a `Grant`) is the capability model's, not a second copy of it.
//! A path readable through the fs but not through the compositor — or
//! the reverse — would be a bug, so there is exactly one implementation.
//!
//! The [`AclEntry`] list remains for direct, capability-free grants
//! (tests and callers that already hold a resolved answer). Entries are
//! unconditional: they union with, and cannot be narrowed by, a bound
//! capability list.

use std::collections::HashSet;

use collab_types::{
    Actor, CapDecision, CapOverride, Ed25519PublicKey, Principal, ResourceRef, Verb,
    decide_capability, path_covers,
};

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

/// The capability list bound to one mount identity.
///
/// Holds the actor the mount acts as and the capabilities authoritative
/// for it. Decisions are **not** precomputed into [`AclEntry`]s: a
/// projection to a flat prefix list cannot express a `Restrict` on a
/// path deeper than a `Grant` (union semantics would keep allowing it),
/// so the capability list is kept whole and
/// [`collab_types::decide_capability`] is asked about the concrete path
/// at check time.
#[derive(Clone, Debug)]
struct CapBinding {
    /// The mount identity these capabilities answer for.
    identity: IdentityKey,
    /// Who the mount acts as, for subject matching.
    actor: Actor,
    /// The capabilities, verbatim — grants, restrictions and expiries.
    caps: Vec<CapOverride>,
    /// The instant capability expiry is evaluated at.
    ///
    /// `None` ⇒ read the system clock on every check, so an
    /// [`collab_types::Expiry::AtUnix`] actually expires part-way through
    /// a long-lived mount. `Some(t)` ⇒ evaluate every check at `t`,
    /// which makes a decision reproducible (and is what tests use).
    ///
    /// Note this is *not* `decide_capability`'s `now: None`, which skips
    /// expiry evaluation entirely — that is right for the wire validator
    /// (two peers share no clock) and wrong here, where skipping it would
    /// silently make every expired capability immortal.
    fixed_now: Option<u64>,
}

impl CapBinding {
    /// Evaluate `access` on `path` through the capability model.
    fn permits(&self, path: &str, access: Access) -> bool {
        let required = match access {
            Access::Read => Verb::Read,
            Access::Write => Verb::Write,
        };
        decide_capability(
            &self.actor,
            &ResourceRef::FsPath(path.to_string()),
            required,
            &self.caps,
            Some(self.fixed_now.unwrap_or_else(unix_now)),
        ) == CapDecision::Allow
    }
}

/// Seconds since the Unix epoch, saturating at 0 if the clock is before it.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A workspace's per-identity access-control list.
///
/// Two sources, both fail-closed:
///
/// - **Capability bindings** ([`WorkspaceAcl::from_capabilities`]) — the
///   production path. Each check delegates to
///   [`collab_types::decide_capability`], so grants, restrictions and
///   expiries behave exactly as they do at every other enforcement point.
/// - **Direct entries** ([`AclEntry`]) — a flat list evaluated with union
///   semantics: an identity holds an access if **any** matching entry
///   grants it. There is no explicit-deny entry; absence *is* denial.
///
/// An empty ACL grants nothing to anyone.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceAcl {
    entries: Vec<AclEntry>,
    bindings: Vec<CapBinding>,
}

impl WorkspaceAcl {
    /// Bind `identity`'s capabilities to this ACL.
    ///
    /// `caps` is the workspace's whole capability list — pass it
    /// unfiltered. Capabilities whose subject does not cover the actor,
    /// whose scope is not a [`collab_types::Scope::FsPath`], or which
    /// have expired are ignored by the decision itself; pre-filtering
    /// here would only risk dropping a `Restrict` that must still bite.
    ///
    /// `subject_principals` are the principals the mount can prove it is
    /// *besides* its own person id (the rooms it is in, the app it runs
    /// as, any bearer token it presented). The person principal derived
    /// from `identity` is always included.
    ///
    /// `now` is seconds since the Unix epoch: `None` uses the live system
    /// clock at each check (the production choice — expiries must bite
    /// during a long mount), `Some(t)` pins every check to `t`.
    ///
    /// The actor deliberately holds **no role**. A role never authorises
    /// a path — [`collab_types::role_base_applies`] returns `false` for
    /// [`collab_types::ResourceRef::FsPath`], so even an Owner reaches
    /// the filesystem only through an explicit capability. Handing the
    /// actor a role here would be inert at best and misleading at worst.
    pub fn from_capabilities(
        identity: IdentityKey,
        caps: &[CapOverride],
        subject_principals: &[Principal],
        now: Option<u64>,
    ) -> Self {
        let mut acl = Self::new();
        acl.bind_capabilities(identity, caps, subject_principals, now);
        acl
    }

    /// Bind another identity's capabilities into an existing ACL.
    ///
    /// Each binding answers only for its own identity, so binding several
    /// is safe — but note [`crate::QuickFS`] serves a single identity per
    /// mount, so only one binding is ever consulted by a live mount.
    pub fn bind_capabilities(
        &mut self,
        identity: IdentityKey,
        caps: &[CapOverride],
        subject_principals: &[Principal],
        now: Option<u64>,
    ) -> &mut Self {
        let key = Ed25519PublicKey(identity);
        let mut actor = Actor {
            role: None,
            key: Some(key),
            principals: vec![Principal::person_from_key(key)],
        };
        for p in subject_principals {
            if !actor.principals.contains(p) {
                actor.principals.push(p.clone());
            }
        }
        self.bindings.push(CapBinding {
            identity,
            actor,
            caps: caps.to_vec(),
            fixed_now: now,
        });
        self
    }

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

    /// The set of identities that appear in the ACL — as a direct grant
    /// or as a capability binding.
    pub fn identities(&self) -> HashSet<IdentityKey> {
        self.entries
            .iter()
            .map(|e| e.identity)
            .chain(self.bindings.iter().map(|b| b.identity))
            .collect()
    }

    /// Does `identity` hold `access` on `path`?
    ///
    /// Fail-closed: returns `true` only if a direct entry for this
    /// identity whose prefix covers `path` grants the requested access,
    /// or a bound capability list decides
    /// [`collab_types::CapDecision::Allow`] for it.
    ///
    /// Path containment is [`collab_types::path_covers`] on both routes —
    /// the same function the capability model matches
    /// [`collab_types::Scope::FsPath`] with. There is no second prefix
    /// rule to drift out of step with.
    pub fn permits(&self, identity: &IdentityKey, path: &str, access: Access) -> bool {
        let direct = self.entries.iter().any(|e| {
            &e.identity == identity
                && path_covers(&e.path_prefix, path)
                && match access {
                    Access::Read => e.read,
                    Access::Write => e.write,
                }
        });
        direct
            || self
                .bindings
                .iter()
                .any(|b| &b.identity == identity && b.permits(path, access))
    }
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

    // The two routes into `permits` — a direct entry and a capability
    // binding — must answer identically for the same subtree. If they
    // ever disagree, a path is readable through one enforcement point
    // and not the other, which is the bug this crate exists to avoid.
    // Both now resolve containment with `collab_types::path_covers`;
    // this proves it on the boundary cases that would expose a drift.
    #[test]
    fn direct_entries_and_capabilities_agree_on_containment() {
        use collab_types::{Effect, Scope, Subject, Verb};

        let mut direct = WorkspaceAcl::new();
        direct.grant_read(ALICE, "/w/docs");

        let cap = CapOverride::new(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath("/w/docs".into()),
            Effect::Grant,
            Verb::Read,
        );
        let capacl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(0));

        for path in [
            "/w/docs",
            "/w/docs/a",
            "/w/docs/a/b.txt",
            "/w/docsSECRET",
            "/w/docs2",
            "/w",
            "/",
            "/other",
        ] {
            assert_eq!(
                direct.permits(&ALICE, path, Access::Read),
                capacl.permits(&ALICE, path, Access::Read),
                "direct entry and capability disagree on read of {path}"
            );
        }
        // …and the shared answer is the right one, not merely consistent.
        assert!(direct.permits(&ALICE, "/w/docs/a/b.txt", Access::Read));
        assert!(!direct.permits(&ALICE, "/w/docsSECRET", Access::Read));
        assert!(capacl.permits(&ALICE, "/w/docs/a/b.txt", Access::Read));
        assert!(!capacl.permits(&ALICE, "/w/docsSECRET", Access::Read));
    }

    // A `Restrict` on a subtree *below* a `Grant` must still bite. This
    // is why capabilities are kept whole rather than projected onto a
    // flat prefix list: union-of-grants would keep allowing the write.
    #[test]
    fn a_deeper_restriction_narrows_a_broader_grant() {
        use collab_types::{Effect, Scope, Subject, Verb};

        let key = Ed25519PublicKey(ALICE);
        let caps = vec![
            CapOverride::new(
                Subject::User(key),
                Scope::FsPath("/w".into()),
                Effect::Grant,
                Verb::Write,
            ),
            CapOverride::new(
                Subject::User(key),
                Scope::FsPath("/w/readonly".into()),
                Effect::Restrict,
                Verb::Read,
            ),
        ];
        let acl = WorkspaceAcl::from_capabilities(ALICE, &caps, &[], Some(0));

        assert!(acl.permits(&ALICE, "/w/notes.txt", Access::Write));
        assert!(acl.permits(&ALICE, "/w/readonly/x.txt", Access::Read));
        assert!(
            !acl.permits(&ALICE, "/w/readonly/x.txt", Access::Write),
            "the Restrict on /w/readonly must survive the broader /w Grant"
        );
    }

    // A room or app principal the mount presents is a subject like any
    // other — the mount need not be named by signing key to hold a cap.
    #[test]
    fn a_presented_principal_is_a_subject() {
        use collab_types::{Effect, Scope, Subject, Verb};

        let cap = CapOverride::new(
            Subject::room("team"),
            Scope::FsPath("/w/team".into()),
            Effect::Grant,
            Verb::Write,
        );
        let member = WorkspaceAcl::from_capabilities(
            ALICE,
            std::slice::from_ref(&cap),
            &[Principal::Room("team".into())],
            Some(0),
        );
        let outsider = WorkspaceAcl::from_capabilities(BOB, std::slice::from_ref(&cap), &[], Some(0));

        assert!(member.permits(&ALICE, "/w/team/plan.md", Access::Write));
        assert!(!outsider.permits(&BOB, "/w/team/plan.md", Access::Read));
    }
}
