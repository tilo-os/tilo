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
//!
//! # Visibility: a denial is an absence
//!
//! [`WorkspaceAcl::permits`] answers "may this identity do this?".
//! [`WorkspaceAcl::visible`] answers the different question the
//! filesystem needs first: **is this path part of the view this identity
//! sees at all?**
//!
//! A path is in the view if the identity may read it, or if it is an
//! ancestor of something the identity may read — an ancestor has to
//! exist or the grant beneath it is unreachable. Everything else is
//! absent, and [`crate::QuickFS`] reports it as absent rather than as
//! refused: it does not appear in a directory listing, and a lookup for
//! it answers `ENOENT`.
//!
//! The distinction matters because `EACCES` is a state most software
//! handles by crashing or by nagging the user, while "the directory
//! contains these three files" is a state every program handles
//! correctly. Presenting a denial as an absence turns the permission
//! system into something applications cannot argue with, and stops
//! training users to click through prompts.
//!
//! Ancestor visibility is decided by asking the *real* decision function
//! about the granted path itself, so a `Restrict` that removes a grant
//! also removes the visibility of the directories that led to it. There
//! is no second, more permissive notion of "readable" here.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use collab_types::{
    Actor, CapDecision, CapOverride, Ed25519PublicKey, Effect, Principal, ResourceRef, Scope, Verb,
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
        self.permits_as(&self.actor, path, access)
    }

    /// [`Self::permits`], but asking on behalf of `actor` rather than the
    /// actor this binding was created for.
    ///
    /// The capability list is the *workspace's*, not one identity's — it
    /// names every subject the workspace has granted anything to — so
    /// asking it about somebody else is reading the same list, not
    /// inventing a second policy. That is what makes one bound list able
    /// to answer for a remote peer as well as for this mount.
    fn permits_as(&self, actor: &Actor, path: &str, access: Access) -> bool {
        let required = match access {
            Access::Read => Verb::Read,
            Access::Write => Verb::Write,
        };
        decide_capability(
            actor,
            &ResourceRef::FsPath(path.to_string()),
            required,
            &self.caps,
            Some(self.fixed_now.unwrap_or_else(unix_now)),
        ) == CapDecision::Allow
    }

    /// The paths this binding's *grants* nominate for reading.
    ///
    /// Candidates only — a nomination is not a decision. They are the
    /// paths worth asking [`Self::permits`] about when deciding whether
    /// some ancestor directory has to exist, which is why a `Restrict`
    /// is not consulted here: it can never introduce a path, only take
    /// one away, and it takes it away in `permits`.
    fn read_nominations(&self) -> impl Iterator<Item = &str> {
        self.read_nominations_for(&self.actor)
    }

    /// [`Self::read_nominations`] for an arbitrary actor.
    fn read_nominations_for<'a>(&'a self, actor: &'a Actor) -> impl Iterator<Item = &'a str> {
        self.caps
            .iter()
            .filter(move |c| {
                c.effect == Effect::Grant
                    && c.verb.implies(Verb::Read)
                    && c.subject.matches_actor(actor)
            })
            .filter_map(|c| match &c.scope {
                Scope::FsPath(p) => Some(p.as_str()),
                _ => None,
            })
    }
}

/// The actor a **remote** identity is evaluated as.
///
/// Its own person principal and nothing else. The principals a mount
/// proves for *itself* — the rooms it is in, the app it runs as, a bearer
/// token it presented — are statements about this machine, and lending
/// them to whoever dials in would let any peer inherit the mount's
/// authority. A peer is only ever the key it authenticated with.
fn peer_actor(peer: &IdentityKey) -> Actor {
    let key = Ed25519PublicKey(*peer);
    Actor {
        role: None,
        key: Some(key),
        principals: vec![Principal::person_from_key(key)],
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

    /// How many capabilities are bound, across every binding. For
    /// logging what a reload put in force.
    pub fn capability_count(&self) -> usize {
        self.bindings.iter().map(|b| b.caps.len()).sum()
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

    /// Does the **remote** identity `peer` hold `access` on `path`?
    ///
    /// The network counterpart of [`Self::permits`]. It exists because
    /// the mount binds capabilities for *its own* identity, and the
    /// question the sync plane has to answer is about somebody else's:
    /// "may the node on the other end of this connection read this?".
    ///
    /// Same list, same [`collab_types::decide_capability`], same
    /// deny-bias — only the actor differs, and it is [`peer_actor`],
    /// which carries nothing but the peer's own key. Asking `permits`
    /// instead would answer `false` for every peer that is not this
    /// mount, which fails closed but also refuses everyone.
    ///
    /// The peer's own identity is evaluated through its bound actor when
    /// it *is* the bound identity, so a mount asking about itself gets
    /// the same answer either way.
    pub fn permits_peer(&self, peer: &IdentityKey, path: &str, access: Access) -> bool {
        let direct = self.entries.iter().any(|e| {
            &e.identity == peer
                && path_covers(&e.path_prefix, path)
                && match access {
                    Access::Read => e.read,
                    Access::Write => e.write,
                }
        });
        if direct {
            return true;
        }
        let peer_actor = peer_actor(peer);
        self.bindings.iter().any(|b| {
            let actor = if &b.identity == peer { &b.actor } else { &peer_actor };
            b.permits_as(actor, path, access)
        })
    }

    /// Does `peer` hold a read grant on *anything* in this workspace?
    ///
    /// The admission question, and deliberately the coarsest one: it is
    /// what the sync and content planes can answer before they know which
    /// path a request will turn out to concern. A peer that reads nothing
    /// here is a stranger, and a stranger is served nothing at all —
    /// not the namespace document, not a blob, not an error that would
    /// tell it the workspace exists.
    ///
    /// A grant is only a candidate until the decision function confirms
    /// it, so a `Restrict` that empties a peer's grants makes this
    /// `false` as surely as never granting anything did.
    pub fn reads_anything(&self, peer: &IdentityKey) -> bool {
        let direct = self
            .entries
            .iter()
            .any(|e| &e.identity == peer && e.read && self.permits_peer(peer, &e.path_prefix, Access::Read));
        if direct {
            return true;
        }
        let peer_actor = peer_actor(peer);
        self.bindings.iter().any(|b| {
            let actor = if &b.identity == peer { &b.actor } else { &peer_actor };
            b.read_nominations_for(actor)
                .any(|granted| b.permits_as(actor, granted, Access::Read))
        })
    }

    /// Is `path` part of the view `identity` sees?
    ///
    /// True when the identity may read `path`, or when `path` is an
    /// ancestor of a path it may read — a granted subtree that cannot be
    /// walked to is not granted at all. False for everything else, and
    /// "false" is what [`crate::QuickFS`] turns into `ENOENT` and into an
    /// omitted directory entry.
    ///
    /// The mount root is always visible. An identity that holds nothing
    /// therefore sees an ordinary empty directory — the state every
    /// program already handles — rather than a mount that errors.
    ///
    /// Ancestry is decided against the *granted* paths, each re-checked
    /// through [`Self::permits`]. A `Restrict` that removes the grant
    /// removes the ancestors with it, so a restriction cannot leave
    /// behind a trail of empty directories pointing at what it hid.
    pub fn visible(&self, identity: &IdentityKey, path: &str) -> bool {
        if path == "/" {
            return true;
        }
        if self.permits(identity, path, Access::Read) {
            return true;
        }
        let direct = self.entries.iter().any(|e| {
            &e.identity == identity
                && e.read
                && path_covers(path, &e.path_prefix)
                && self.permits(identity, &e.path_prefix, Access::Read)
        });
        direct
            || self.bindings.iter().any(|b| {
                &b.identity == identity
                    && b.read_nominations().any(|granted| {
                        path_covers(path, granted)
                            && self.permits(identity, granted, Access::Read)
                    })
            })
    }
}

/// The ACL a live mount enforces, in a form that can be replaced while
/// the mount is up.
///
/// A [`WorkspaceAcl`] is a snapshot of a decision. Handing that snapshot
/// to [`crate::QuickFS`] by value made the mount's policy fixed at mount
/// time: a capability granted (or revoked) a minute later did not reach
/// the filesystem until the next remount, so the recorded grant and the
/// enforced grant disagreed for as long as the mount lived. That is the
/// same class of bug the capability model exists to prevent, only with
/// the drift in time rather than in vocabulary.
///
/// So the mount holds a handle, and whoever owns the policy source —
/// [`crate::CapabilityFile`] in production — swaps the contents under it.
/// Reads take the lock per access; a `RwLock` read is uncontended in the
/// steady state and is dwarfed by the FUSE round-trip it sits inside.
#[derive(Clone, Debug)]
pub struct AclHandle(Arc<RwLock<WorkspaceAcl>>);

impl AclHandle {
    /// A handle over `acl`.
    pub fn new(acl: WorkspaceAcl) -> Self {
        Self(Arc::new(RwLock::new(acl)))
    }

    /// Does `identity` hold `access` on `path`, per the ACL bound *right
    /// now*? Delegates to [`WorkspaceAcl::permits`] — there is no second
    /// decision here, only a load.
    pub fn permits(&self, identity: &IdentityKey, path: &str, access: Access) -> bool {
        // A poisoned lock means a writer panicked mid-swap. Fail closed:
        // the alternative is to answer from a value nobody vouches for.
        match self.0.read() {
            Ok(acl) => acl.permits(identity, path, access),
            Err(_) => false,
        }
    }

    /// Does the remote identity `peer` hold `access` on `path`, per the
    /// ACL bound *right now*? Delegates to [`WorkspaceAcl::permits_peer`].
    pub fn permits_peer(&self, peer: &IdentityKey, path: &str, access: Access) -> bool {
        match self.0.read() {
            Ok(acl) => acl.permits_peer(peer, path, access),
            Err(_) => false,
        }
    }

    /// Does `peer` read anything in this workspace, per the ACL bound
    /// *right now*? Delegates to [`WorkspaceAcl::reads_anything`].
    ///
    /// Read live rather than captured at connect time so a revocation
    /// reaches the network plane on the next request, the same way it
    /// already reaches the FUSE plane on the next `open`.
    pub fn reads_anything(&self, peer: &IdentityKey) -> bool {
        match self.0.read() {
            Ok(acl) => acl.reads_anything(peer),
            Err(_) => false,
        }
    }

    /// Is `path` part of the view `identity` sees, per the ACL bound
    /// *right now*? Delegates to [`WorkspaceAcl::visible`].
    ///
    /// Visibility has to be read through the same live handle as
    /// [`Self::permits`], not from a snapshot taken at mount time: a
    /// revocation that stops permitting a path but keeps listing it turns
    /// denial back into a signal, which is exactly what deciding both
    /// against one bound ACL prevents.
    pub fn visible(&self, identity: &IdentityKey, path: &str) -> bool {
        // Fail closed on a poisoned lock, as `permits` does — an
        // unvouched-for ACL must hide, not reveal.
        match self.0.read() {
            Ok(acl) => acl.visible(identity, path),
            Err(_) => false,
        }
    }

    /// Replace the bound ACL. Every access after this call is decided by
    /// `acl`; accesses already in flight finish against the old one.
    pub fn replace(&self, acl: WorkspaceAcl) {
        match self.0.write() {
            Ok(mut slot) => *slot = acl,
            // Re-establish a known value rather than leaving the mount
            // deciding from a poisoned lock (which `permits` denies on).
            Err(poisoned) => *poisoned.into_inner() = acl,
        }
    }

    /// The currently-bound ACL, cloned. For tests and for logging.
    pub fn snapshot(&self) -> WorkspaceAcl {
        match self.0.read() {
            Ok(acl) => acl.clone(),
            Err(_) => WorkspaceAcl::new(),
        }
    }
}

impl From<WorkspaceAcl> for AclHandle {
    fn from(acl: WorkspaceAcl) -> Self {
        Self::new(acl)
    }
}

impl From<&AclHandle> for AclHandle {
    fn from(handle: &AclHandle) -> Self {
        handle.clone()
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

    // The view is the granted subtree plus the directories that lead to
    // it, and nothing else. The ancestors have to be there or the grant
    // cannot be walked to; the siblings must not be, or the denial is a
    // refusal rather than an absence.
    #[test]
    fn the_view_is_the_grant_plus_the_path_to_it() {
        use collab_types::{Effect, Scope, Subject, Verb};

        let cap = CapOverride::new(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath("/w/docs".into()),
            Effect::Grant,
            Verb::Read,
        );
        let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(0));

        for present in ["/", "/w", "/w/docs", "/w/docs/a", "/w/docs/a/b.txt"] {
            assert!(
                acl.visible(&ALICE, present),
                "{present} must be part of the view"
            );
        }
        for absent in ["/w/other", "/w/other/private.md", "/w/docsSECRET", "/elsewhere"] {
            assert!(
                !acl.visible(&ALICE, absent),
                "{absent} must be absent from the view, not merely denied"
            );
        }
    }

    // An identity holding nothing sees an empty root, not a broken
    // mount. "The folder is empty" is a state every program handles.
    #[test]
    fn an_identity_with_no_grant_sees_an_empty_root() {
        let acl = WorkspaceAcl::new();
        assert!(acl.visible(&ALICE, "/"), "the mount root always exists");
        assert!(!acl.visible(&ALICE, "/anything"));
        assert!(!acl.visible(&ALICE, "/w/docs"));
    }

    // A `Restrict` narrows what may be done, never what may be seen:
    // every verb implies `Read`, so no restriction can take reading away
    // (see `decide_actor`). A restricted subtree therefore stays in the
    // view and becomes read-only.
    //
    // The consequence is worth stating, because it is what makes the
    // filtered view safe to write into: **inside a writable subtree,
    // nothing is hidden.** A capability cannot produce a name that is
    // absent from the listing but present in the tree, so an app that
    // holds write on a directory cannot discover a hidden sibling by
    // trying to create it.
    #[test]
    fn a_restriction_narrows_writing_without_hiding_anything() {
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

        assert!(acl.visible(&ALICE, "/w/readonly/x.txt"));
        assert!(acl.permits(&ALICE, "/w/readonly/x.txt", Access::Read));
        assert!(!acl.permits(&ALICE, "/w/readonly/x.txt", Access::Write));
        // …and everything the write grant covers is visible, so there is
        // no name in it that the listing omits.
        for path in ["/w", "/w/notes.txt", "/w/readonly", "/w/sub/deep.txt"] {
            assert!(
                acl.visible(&ALICE, path),
                "{path} is inside the write grant and must be in the view"
            );
        }
    }

    // An expired grant nominates nothing: the ancestor test re-runs the
    // real decision, so expiry removes the trail as well as the target.
    #[test]
    fn an_expired_grant_leaves_no_visible_ancestors() {
        use collab_types::{Effect, Expiry, Scope, Subject, Verb};

        let cap = CapOverride::new(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath("/w/docs".into()),
            Effect::Grant,
            Verb::Read,
        )
        .expiring(Expiry::AtUnix(500));

        let live = WorkspaceAcl::from_capabilities(ALICE, std::slice::from_ref(&cap), &[], Some(499));
        assert!(live.visible(&ALICE, "/w"), "while live, the trail exists");

        let lapsed = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(501));
        assert!(!lapsed.visible(&ALICE, "/w/docs"));
        assert!(!lapsed.visible(&ALICE, "/w"));
    }

    // Write-only-looking grants still make the subtree visible: `Write`
    // implies `Read`, so a writable subtree is a readable one and its
    // ancestors must exist.
    #[test]
    fn a_write_grant_is_visible_and_so_are_its_ancestors() {
        use collab_types::{Effect, Scope, Subject, Verb};

        let cap = CapOverride::new(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath("/w/inbox".into()),
            Effect::Grant,
            Verb::Write,
        );
        let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(0));
        assert!(acl.visible(&ALICE, "/w"));
        assert!(acl.visible(&ALICE, "/w/inbox/note.md"));
        assert!(!acl.visible(&ALICE, "/w/outbox"));
    }

    // Direct entries take the same route as capabilities, so a
    // capability-free grant also produces walkable ancestors.
    #[test]
    fn direct_entries_produce_visible_ancestors_too() {
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(ALICE, "/w/docs");
        assert!(acl.visible(&ALICE, "/w"));
        assert!(acl.visible(&ALICE, "/w/docs/report.md"));
        assert!(!acl.visible(&ALICE, "/w/other"));
        assert!(!acl.visible(&BOB, "/w"));
    }

    use collab_types::Subject;

    // ---- the network's question: may *that* peer read anything here? ----
    //
    // The mount binds capabilities for its own identity, so `permits`
    // answers `false` for everyone else — correct for the FUSE path,
    // useless for deciding who may open a connection. `permits_peer` and
    // `reads_anything` ask the same capability list about a remote key.

    /// A capability list written for Bob decides for Bob, even on a mount
    /// bound as Alice.
    #[test]
    fn a_peer_is_decided_by_the_workspaces_own_capability_list() {
        let cap = CapOverride::new(
            Subject::User(Ed25519PublicKey(BOB)),
            Scope::FsPath("/docs".into()),
            Effect::Grant,
            Verb::Read,
        );
        let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(0));
        assert!(acl.permits_peer(&BOB, "/docs/report.md", Access::Read));
        assert!(acl.reads_anything(&BOB));
        // The grant is a read grant and stays one.
        assert!(!acl.permits_peer(&BOB, "/docs/report.md", Access::Write));
        // And it does not reach outside its subtree.
        assert!(!acl.permits_peer(&BOB, "/secrets", Access::Read));
    }

    /// An identity the list never names reads nothing — which is what the
    /// sync and content planes turn into "served nothing".
    #[test]
    fn an_unnamed_peer_reads_nothing() {
        let cap = CapOverride::new(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath("/".into()),
            Effect::Grant,
            Verb::Read,
        );
        let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(0));
        assert!(acl.reads_anything(&ALICE), "the bound identity still reads");
        assert!(!acl.reads_anything(&BOB));
        assert!(!acl.permits_peer(&BOB, "/", Access::Read));
        assert!(
            !WorkspaceAcl::new().reads_anything(&BOB),
            "an empty ACL admits nobody"
        );
    }

    /// Revocation reaches the network plane, because the plane asks the
    /// live handle rather than a snapshot taken when the mount came up.
    ///
    /// This is the same swap [`crate::CapabilityFile::refresh`] performs
    /// when the capability file is emptied or removed, so a peer that
    /// loses its grant stops being served without a remount.
    #[test]
    fn revoking_a_peers_grant_stops_it_reading() {
        let cap = CapOverride::new(
            Subject::User(Ed25519PublicKey(BOB)),
            Scope::FsPath("/docs".into()),
            Effect::Grant,
            Verb::Read,
        );
        let handle = AclHandle::new(WorkspaceAcl::from_capabilities(
            ALICE,
            &[cap],
            &[],
            Some(0),
        ));
        assert!(handle.reads_anything(&BOB));
        assert!(handle.permits_peer(&BOB, "/docs/report.md", Access::Read));

        handle.replace(WorkspaceAcl::new());
        assert!(!handle.reads_anything(&BOB));
        assert!(!handle.permits_peer(&BOB, "/docs/report.md", Access::Read));
    }

    /// A peer does not inherit the principals this *mount* proves for
    /// itself. A capability granted to a room the mount is in is the
    /// mount's, and lending it to whoever dials in would hand every
    /// caller the host's authority.
    #[test]
    fn a_peer_does_not_inherit_the_mounts_principals() {
        let room = Principal::Room("6b3f9a10-0000-4000-8000-00000000000a".to_string());
        let cap = CapOverride::new(
            Subject::Principal(room.clone()),
            Scope::FsPath("/".into()),
            Effect::Grant,
            Verb::Read,
        );
        let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[room], Some(0));
        assert!(acl.permits(&ALICE, "/x", Access::Read), "the mount holds it");
        assert!(!acl.reads_anything(&BOB), "a caller does not");
        assert!(!acl.permits_peer(&BOB, "/x", Access::Read));
    }
}
