//! Per-identity workspace ACL — the filesystem enforcement point.
//!
//! The FUSE driver is *the* enforcement point for file access: every
//! open/read/write is checked against the workspace ACL keyed on
//! **identity** (a 32-byte public key), not Unix mode bits, which stay a
//! compatibility veneer. [`QuickFS`](crate::QuickFS) is bound to one
//! identity per mount and, once an ACL is attached, checks every access
//! against it.
//!
//! **Fail closed.** An identity has access only if some entry explicitly
//! grants it. A mount with no ACL attached keeps Unix-veneer-only
//! behaviour, so unshared single-user mounts are unaffected — the ACL is
//! opt-in, and once attached it is authoritative.
//!
//! # Capabilities are the source of truth
//!
//! The system's one permission vocabulary is
//! [`collab_types::CapOverride`] — `(subject × scope × verb)` plus an
//! expiry. [`WorkspaceAcl::from_capabilities`] binds a capability list to
//! a mount, and every `permits` call is answered by
//! [`collab_types::decide_capability`] itself: containment, expiry and
//! deny-bias are the capability model's, never re-derived here, so a
//! path readable through the fs but not the compositor (or the reverse)
//! can't happen from drift.
//!
//! [`AclEntry`] remains for direct, capability-free grants (tests and
//! callers with an already-resolved answer). Entries union with, and
//! cannot be narrowed by, a bound capability list.
//!
//! # Visibility: a denial is an absence
//!
//! [`WorkspaceAcl::permits`] answers "may this identity do this?".
//! [`WorkspaceAcl::visible`] answers what the filesystem needs first: is
//! this path part of the identity's view at all? A path is in view if
//! the identity may read it, or is an ancestor of something it may read
//! (the ancestor must exist or the grant beneath it is unreachable).
//! Everything else is absent — [`crate::QuickFS`] omits it from a
//! listing and answers `ENOENT`, not `EACCES`, since absence is a state
//! every program already handles.
//!
//! Ancestor visibility asks the *real* decision function about the
//! granted path itself, so a `Restrict` that removes a grant removes its
//! ancestors' visibility too.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use collab_types::{
    Access as CapAccess, Actor, CapDecision, CapOverride, Ed25519PublicKey, Effect, Principal,
    ResourceRef, Scope, decide_capability, path_covers,
};

/// A Tilo identity — the 32-byte Ed25519 public key that anchors a
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
/// `path_prefix` matches the path itself and everything beneath it:
/// `"/"` matches everything; `"/docs"` matches `/docs` and `/docs/**` but
/// not `/docsxyz`.
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

/// The capability list bound to one mount identity. Holds the actor the
/// mount acts as and the capabilities authoritative for it. Decisions
/// are **not** precomputed into [`AclEntry`]s: a flat-prefix-list
/// projection can't express a `Restrict` deeper than a `Grant` (union
/// semantics would keep allowing it), so the capability list stays whole
/// and [`collab_types::decide_capability`] is asked about the concrete
/// path at check time.
#[derive(Clone, Debug)]
struct CapBinding {
    /// The mount identity these capabilities answer for.
    identity: IdentityKey,
    /// Who the mount acts as, for subject matching.
    actor: Actor,
    /// The complete capabilities with Files mount paths projected into this
    /// namespace — grants, restrictions, subjects and expiries stay intact.
    caps: Vec<CapOverride>,
    /// The instant capability expiry is evaluated at. `None` reads the
    /// system clock every check, so an [`collab_types::Expiry::AtUnix`]
    /// expires mid-mount; `Some(t)` pins every check for
    /// reproducible tests. Distinct from `decide_capability`'s `now: None`
    /// (which skips expiry entirely — right for the wire validator, wrong
    /// here, since it would make every expired capability immortal).
    fixed_now: Option<u64>,
}

impl CapBinding {
    /// Evaluate `access` on `path` through the capability model.
    fn permits(&self, path: &str, access: Access) -> bool {
        self.permits_as(&self.actor, path, access)
    }

    /// [`Self::permits`] asked on behalf of `actor` rather than the actor
    /// this binding was created for — the capability list is the
    /// *workspace's*, so asking about somebody else reads the same list, not
    /// a second policy.
    fn permits_as(&self, actor: &Actor, path: &str, access: Access) -> bool {
        let required = match access {
            Access::Read => CapAccess::Read,
            Access::Write => CapAccess::Write,
        };
        decide_capability(
            actor,
            &ResourceRef::FsPath(path.to_string()),
            required,
            &self.caps,
            Some(self.fixed_now.unwrap_or_else(crate::clock::unix_now_secs)),
        ) == CapDecision::Allow
    }

    /// The paths this binding's *grants* nominate for reading — candidates
    /// worth asking [`Self::permits`] about when deciding whether an
    /// ancestor directory has to exist. A `Restrict` isn't consulted here
    /// since it can only take a path away, in `permits`.
    fn read_nominations(&self) -> impl Iterator<Item = &str> {
        self.read_nominations_for(&self.actor)
    }

    /// [`Self::read_nominations`] for an arbitrary actor.
    fn read_nominations_for<'a>(&'a self, actor: &'a Actor) -> impl Iterator<Item = &'a str> {
        self.caps
            .iter()
            .filter(move |c| {
                c.effect == Effect::Grant
                    && c.access.implies(CapAccess::Read)
                    && c.subject.matches_actor(actor)
            })
            .filter_map(|c| match &c.scope {
                Scope::FsPath(p) => Some(p.as_str()),
                _ => None,
            })
    }
}

/// The normal Permissions surface and application sandbox name Files here.
/// A signed workspace authority binds this mount's policy to its namespace UUID.
const WORKSPACE_FILES_PATH: &str = "/var/workspace";

/// Project only the path coordinate; never rewrite the signed policy or grant.
/// Ancestor restrictions must map too, so a ceiling on `/var` still restricts
/// the Files subtree rather than disappearing at the mount boundary.
fn namespace_capabilities(caps: &[CapOverride]) -> Vec<CapOverride> {
    caps.iter()
        .cloned()
        .map(|mut cap| {
            if let Scope::FsPath(path) = &mut cap.scope {
                if path_covers(path, WORKSPACE_FILES_PATH) {
                    *path = "/".into();
                } else if let Some(relative) = path.strip_prefix("/var/workspace/") {
                    *path = format!("/{relative}");
                }
            }
            cap
        })
        .collect()
}

/// The actor a **remote** identity is evaluated as: its own person
/// principal and nothing else. The principals a mount proves for itself
/// (rooms, app identity, bearer tokens) are statements about this
/// machine — lending them to a dialer would let any peer inherit the
/// mount's authority, so a peer is only ever the key it authenticated with.
fn peer_actor(peer: &IdentityKey) -> Actor {
    let key = Ed25519PublicKey(*peer);
    Actor {
        key: Some(key),
        principals: vec![Principal::person_from_key(key)],
    }
}

/// Seconds since the Unix epoch, saturating at 0 if the clock is before it.
/// A workspace's per-identity access-control list. Two sources, both
/// fail-closed: **capability bindings**
/// ([`WorkspaceAcl::from_capabilities`], the production path) delegate
/// to [`collab_types::decide_capability`], so grants/restrictions/
/// expiries behave exactly as at every other enforcement point; **direct
/// entries** ([`AclEntry`]) are a flat list with union semantics — any
/// matching entry grants access, absence *is* denial (no explicit-deny
/// entry). An empty ACL grants nothing to anyone.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceAcl {
    entries: Vec<AclEntry>,
    bindings: Vec<CapBinding>,
}

impl WorkspaceAcl {
    /// Bind `identity`'s capabilities to this ACL.
    ///
    /// Paths beneath the normal `/var/workspace` projection are evaluated
    /// relative to this namespace root. Ancestor scopes map to the root too;
    /// existing namespace-relative scopes remain supported. The caller's signed
    /// list is never modified, and no host filesystem path is opened here.
    ///
    /// `caps` is the workspace's whole capability list, unfiltered:
    /// capabilities that don't cover the actor, aren't
    /// [`collab_types::Scope::FsPath`], or have expired are ignored by the
    /// decision itself — pre-filtering here would risk dropping a `Restrict`
    /// that must still bite.
    ///
    /// `subject_principals` are principals the mount can prove besides its
    /// own person id; the person principal from `identity` is always
    /// included. `now` is seconds since epoch: `None` uses the live clock,
    /// `Some(t)` pins every check.
    ///
    /// A room role never authorises a path: even an owner reaches the
    /// filesystem only through an explicit sandbox capability.
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

    /// Bind another identity's capabilities into an existing ACL. Each
    /// binding answers only for its own identity, so binding several is
    /// safe — though [`crate::QuickFS`] serves a single identity per mount,
    /// so only one binding is ever consulted live.
    pub fn bind_capabilities(
        &mut self,
        identity: IdentityKey,
        caps: &[CapOverride],
        subject_principals: &[Principal],
        now: Option<u64>,
    ) -> &mut Self {
        let key = Ed25519PublicKey(identity);
        let mut actor = Actor {
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
            caps: namespace_capabilities(caps),
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
    pub fn grant_read(
        &mut self,
        identity: IdentityKey,
        path_prefix: impl Into<String>,
    ) -> &mut Self {
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

    /// How many capabilities are bound, across every binding — for logging
    /// what a reload put in force.
    pub fn capability_count(&self) -> usize {
        self.bindings.iter().map(|b| b.caps.len()).sum()
    }

    /// The set of identities appearing in the ACL, as a direct grant or a
    /// capability binding.
    pub fn identities(&self) -> HashSet<IdentityKey> {
        self.entries
            .iter()
            .map(|e| e.identity)
            .chain(self.bindings.iter().map(|b| b.identity))
            .collect()
    }

    /// Whether anyone but the mount itself may be named here: a direct
    /// entry for an identity no binding answers for, or a capability whose
    /// subject is not the bound actor (another person, a role, a token, a
    /// room the mount does not prove). The shipped list, which grants the
    /// mount's own principal, names nobody else: then only the mount's own
    /// identity (the person's other devices) can be admitted, never another
    /// party ([`crate::reachability`]).
    #[must_use]
    pub fn names_others(&self) -> bool {
        let own: HashSet<IdentityKey> = self.bindings.iter().map(|b| b.identity).collect();
        self.entries.iter().any(|e| !own.contains(&e.identity))
            || self.bindings.iter().any(|b| {
                b.caps
                    .iter()
                    .any(|cap| !cap.subject.matches_actor(&b.actor))
            })
    }

    /// Does `identity` hold `access` on `path`? Fail-closed: `true` only if
    /// a direct entry whose prefix covers `path` grants it, or a bound
    /// capability list decides `Allow`. Path containment is
    /// [`collab_types::path_covers`] on both routes — the same function the
    /// capability model matches `FsPath` with, so there's no second prefix
    /// rule to drift out of step.
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

    /// Does the **remote** identity `peer` hold `access` on `path`? The
    /// network counterpart of [`Self::permits`] (which is `false` for every
    /// peer, since it only answers for the mount's own identity). Same list,
    /// same `decide_capability`, same deny-bias; only the actor differs — for
    /// a peer it's `peer_actor`, unless a binding exists *for* that peer.
    pub fn permits_peer(&self, peer: &IdentityKey, path: &str, access: Access) -> bool {
        let direct = self.entries.iter().any(|e| {
            &e.identity == peer
                && path_covers(&e.path_prefix, path)
                && match access {
                    Access::Read => e.read,
                    Access::Write => e.write,
                }
        });
        direct || {
            let as_peer = peer_actor(peer);
            self.bindings.iter().any(|b| {
                let actor = if &b.identity == peer {
                    &b.actor
                } else {
                    &as_peer
                };
                b.permits_as(actor, path, access)
            })
        }
    }

    /// Does `peer` hold a read grant on *anything* here? The admission
    /// question, deliberately the coarsest one — what the sync and content
    /// planes can answer before knowing which path a request will concern. A
    /// peer that reads nothing is a stranger, served nothing at all. A
    /// `Restrict` that empties a peer's grants makes this `false` just as
    /// surely as never granting anything did.
    pub fn reads_anything(&self, peer: &IdentityKey) -> bool {
        let direct = self.entries.iter().any(|e| &e.identity == peer && e.read);
        direct || {
            let as_peer = peer_actor(peer);
            self.bindings.iter().any(|b| {
                let actor = if &b.identity == peer {
                    &b.actor
                } else {
                    &as_peer
                };
                b.read_nominations_for(actor)
                    .any(|granted| b.permits_as(actor, granted, Access::Read))
            })
        }
    }

    /// Is `path` part of the view `identity` sees? True when the identity
    /// may read `path`, or `path` is an ancestor of a path it may read.
    /// False otherwise — [`crate::QuickFS`] turns that into `ENOENT` and an
    /// omitted directory entry. The mount root is always visible, so an
    /// identity holding nothing sees an empty directory, not an erroring mount.
    ///
    /// Ancestry is decided against the *granted* paths, each re-checked
    /// through [`Self::permits`], so a `Restrict` that removes a grant
    /// removes its ancestors' visibility too.
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
                        path_covers(path, granted) && self.permits(identity, granted, Access::Read)
                    })
            })
    }
}

/// The ACL a live mount enforces, replaceable while the mount is up.
/// Handing a [`WorkspaceAcl`] snapshot to [`crate::QuickFS`] by value
/// would fix the policy at mount time — a capability granted or revoked
/// later wouldn't reach the filesystem until remount, the drift bug the
/// capability model exists to prevent. So the mount holds a handle;
/// whoever owns the policy source ([`crate::CapabilityFile`] in
/// production) swaps the contents under it — an uncontended `RwLock`
/// read, dwarfed by the FUSE round-trip it sits inside.
#[derive(Clone, Debug)]
pub struct AclHandle {
    acl: Arc<RwLock<WorkspaceAcl>>,
    revision: Arc<std::sync::atomic::AtomicU64>,
    revision_watch: tokio::sync::watch::Sender<u64>,
}

impl AclHandle {
    pub(crate) fn admission_lease(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, WorkspaceAcl>, ()> {
        self.acl.read().map_err(|_| ())
    }

    /// A handle over `acl`.
    pub fn new(acl: WorkspaceAcl) -> Self {
        let (revision_watch, _) = tokio::sync::watch::channel(0_u64);
        Self {
            acl: Arc::new(RwLock::new(acl)),
            revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            revision_watch,
        }
    }

    /// Does `identity` hold `access` on `path`, per the ACL bound *right
    /// now*? Delegates to [`WorkspaceAcl::permits`] — no second decision, only a load.
    pub fn permits(&self, identity: &IdentityKey, path: &str, access: Access) -> bool {
        // A poisoned lock means a writer panicked mid-swap. Fail closed: the
        // alternative is answering from a value nobody vouches for.
        match self.acl.read() {
            Ok(acl) => acl.permits(identity, path, access),
            Err(_) => false,
        }
    }

    /// Does the remote identity `peer` hold `access` on `path`, per the ACL
    /// bound *right now*? Delegates to [`WorkspaceAcl::permits_peer`].
    pub fn permits_peer(&self, peer: &IdentityKey, path: &str, access: Access) -> bool {
        match self.acl.read() {
            Ok(acl) => acl.permits_peer(peer, path, access),
            Err(_) => false,
        }
    }

    /// Does `peer` read anything here, per the ACL bound *right now*?
    /// Delegates to [`WorkspaceAcl::reads_anything`], read live (not
    /// captured at connect time) so a revocation reaches the network plane
    /// on the next request, same as it already reaches FUSE on the next `open`.
    pub fn reads_anything(&self, peer: &IdentityKey) -> bool {
        match self.acl.read() {
            Ok(acl) => acl.reads_anything(peer),
            Err(_) => false,
        }
    }

    /// Is `path` part of `identity`'s view, per the ACL bound *right now*?
    /// Delegates to [`WorkspaceAcl::visible`] through the same live handle as
    /// [`Self::permits`], not a snapshot — otherwise a revocation could stop
    /// permitting a path while still listing it, turning denial back into a signal.
    pub fn visible(&self, identity: &IdentityKey, path: &str) -> bool {
        // Fail closed on a poisoned lock, as `permits` does — an
        // unvouched-for ACL must hide, not reveal.
        match self.acl.read() {
            Ok(acl) => acl.visible(identity, path),
            Err(_) => false,
        }
    }

    /// Replace the bound ACL. Every access after this call is decided by
    /// `acl`; accesses already in flight finish against the old one.
    pub fn replace(&self, acl: WorkspaceAcl) {
        match self.acl.write() {
            Ok(mut slot) => *slot = acl,
            // Re-establish a known value rather than leaving the mount deciding
            // from a poisoned lock (which `permits` denies on).
            Err(poisoned) => *poisoned.into_inner() = acl,
        }
        self.revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        self.revision_watch
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// Monotonic policy generation for ACL-filtered derived projections.
    pub fn revision(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Subscribe to policy replacements without polling the filesystem. The
    /// latest generation is sufficient because derived ACL views coalesce
    /// intermediate replacements before rebuilding.
    pub fn subscribe_revision(&self) -> tokio::sync::watch::Receiver<u64> {
        self.revision_watch.subscribe()
    }

    /// The currently-bound ACL, cloned. For tests and for logging.
    pub fn snapshot(&self) -> WorkspaceAcl {
        match self.acl.read() {
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
    use collab_types::Subject;

    const ALICE: IdentityKey = [1u8; 32];
    const BOB: IdentityKey = [2u8; 32];

    #[test]
    fn normal_files_mount_scope_preserves_policy_and_deny_bias() {
        let grant = CapOverride::grant(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath("/var/workspace".into()),
            CapAccess::Write,
        );
        let mut restriction = grant.clone();
        restriction.scope = Scope::FsPath("/var/workspace/private".into());
        restriction.effect = Effect::Restrict;
        restriction.access = CapAccess::Read;
        let caps = vec![grant, restriction];
        let original = caps.clone();
        let acl = WorkspaceAcl::from_capabilities(ALICE, &caps, &[], Some(0));
        for path in ["/", "/notes.md", "/private/notes.md"] {
            for (verb, access) in [
                (CapAccess::Read, Access::Read),
                (CapAccess::Write, Access::Write),
            ] {
                let mounted = format!("/var/workspace{}", if path == "/" { "" } else { path });
                let expected = decide_capability(
                    &peer_actor(&ALICE),
                    &ResourceRef::FsPath(mounted),
                    verb,
                    &caps,
                    Some(0),
                ) == CapDecision::Allow;
                assert_eq!(acl.permits(&ALICE, path, access), expected);
                assert_eq!(acl.permits_peer(&ALICE, path, access), expected);
            }
        }
        assert_eq!(caps, original, "signed policy is not rewritten");
        assert!(!acl.permits_peer(&BOB, "/", Access::Read));
        for outside in ["/", "/etc/passwd", "/var/workspace-other/notes.md"] {
            assert_ne!(
                decide_capability(
                    &peer_actor(&ALICE),
                    &ResourceRef::FsPath(outside.into()),
                    CapAccess::Read,
                    &caps,
                    Some(0),
                ),
                CapDecision::Allow,
                "no host-root permission was granted"
            );
        }
        let mut ancestor = caps[1].clone();
        ancestor.scope = Scope::FsPath("/var".into());
        let restricted =
            WorkspaceAcl::from_capabilities(ALICE, &[caps[0].clone(), ancestor], &[], Some(0));
        assert!(restricted.permits(&ALICE, "/notes.md", Access::Read));
        assert!(!restricted.permits(&ALICE, "/notes.md", Access::Write));
    }

    #[test]
    fn normal_files_subtree_visibility_expiry_and_revocation_remain_scoped() {
        let grant = CapOverride::grant(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath("/var/workspace/team/documents".into()),
            CapAccess::Read,
        )
        .with_expiry(20);
        let acl =
            WorkspaceAcl::from_capabilities(ALICE, std::slice::from_ref(&grant), &[], Some(10));
        assert!(!acl.permits(&ALICE, "/", Access::Read));
        assert!(acl.visible(&ALICE, "/team"));
        assert!(acl.permits_peer(&ALICE, "/team/documents/a.txt", Access::Read));
        assert!(!acl.permits_peer(&ALICE, "/team/documents-secret/a.txt", Access::Read));
        assert!(!acl.visible(&ALICE, "/other"));
        assert!(acl.reads_anything(&ALICE));
        assert!(!acl.reads_anything(&BOB));
        let expired =
            WorkspaceAcl::from_capabilities(ALICE, std::slice::from_ref(&grant), &[], Some(21));
        assert!(!expired.visible(&ALICE, "/team"));
        assert!(!expired.reads_anything(&ALICE));
        let handle = AclHandle::new(acl);
        handle.replace(WorkspaceAcl::from_capabilities(ALICE, &[], &[], Some(10)));
        assert!(!handle.permits(&ALICE, "/team/documents/a.txt", Access::Read));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn permits_follows_grants_restrictions_and_capabilities_by_path_component() {
        // Phase `empty_acl_denies_everyone`.
        {
            let acl = WorkspaceAcl::new();
            assert!(!acl.permits(&ALICE, "/anything", Access::Read));
            assert!(!acl.permits(&ALICE, "/", Access::Write));
        }
        // Phase `read_grant_allows_read_not_write`.
        {
            let mut acl = WorkspaceAcl::new();
            acl.grant_read(ALICE, "/docs");
            assert!(acl.permits(&ALICE, "/docs", Access::Read));
            assert!(acl.permits(&ALICE, "/docs/report.md", Access::Read));
            assert!(!acl.permits(&ALICE, "/docs", Access::Write));
            // Bob has no grant at all.
            assert!(!acl.permits(&BOB, "/docs/report.md", Access::Read));
        }
        // Phase `prefix_respects_component_boundary`.
        {
            let mut acl = WorkspaceAcl::new();
            acl.grant_read(ALICE, "/doc");
            assert!(acl.permits(&ALICE, "/doc/a", Access::Read));
            assert!(acl.permits(&ALICE, "/doc", Access::Read));
            // Not a subtree of "/doc" — must not leak.
            assert!(!acl.permits(&ALICE, "/document", Access::Read));
        }
        // Phase `root_prefix_covers_all`.
        {
            let mut acl = WorkspaceAcl::new();
            acl.grant_read_write(ALICE, "/");
            assert!(acl.permits(&ALICE, "/x/y/z", Access::Read));
            assert!(acl.permits(&ALICE, "/x/y/z", Access::Write));
        }
        // Phase `direct_entries_and_capabilities_agree_on_containment`.
        // The two routes into `permits` — a direct entry and a capability
        // binding — must answer identically for the same subtree, or a path
        // is readable through one enforcement point and not the other, the
        // bug this crate exists to avoid. Both resolve containment with
        // `collab_types::path_covers`; this proves it on boundary cases.
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let mut direct = WorkspaceAcl::new();
            direct.grant_read(ALICE, "/w/docs");

            let cap = CapOverride::new(
                Subject::User(Ed25519PublicKey(ALICE)),
                Scope::FsPath("/w/docs".into()),
                Effect::Grant,
                CapAccess::Read,
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
        // Phase `a_deeper_restriction_narrows_a_broader_grant`.
        // A `Restrict` on a subtree *below* a `Grant` must still bite — why
        // capabilities are kept whole rather than projected onto a flat
        // prefix list (union-of-grants would keep allowing the write).
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let key = Ed25519PublicKey(ALICE);
            let caps = vec![
                CapOverride::new(
                    Subject::User(key),
                    Scope::FsPath("/w".into()),
                    Effect::Grant,
                    CapAccess::Write,
                ),
                CapOverride::new(
                    Subject::User(key),
                    Scope::FsPath("/w/readonly".into()),
                    Effect::Restrict,
                    CapAccess::Read,
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
        // Phase `a_presented_principal_is_a_subject`.
        // A room or app principal the mount presents is a subject like any
        // other — the mount need not be named by signing key to hold a cap.
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let cap = CapOverride::new(
                Subject::room("team"),
                Scope::FsPath("/w/team".into()),
                Effect::Grant,
                CapAccess::Write,
            );
            let member = WorkspaceAcl::from_capabilities(
                ALICE,
                std::slice::from_ref(&cap),
                &[Principal::Room("team".into())],
                Some(0),
            );
            let outsider =
                WorkspaceAcl::from_capabilities(BOB, std::slice::from_ref(&cap), &[], Some(0));

            assert!(member.permits(&ALICE, "/w/team/plan.md", Access::Write));
            assert!(!outsider.permits(&BOB, "/w/team/plan.md", Access::Read));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn the_visible_view_is_each_grant_plus_its_ancestors() {
        // Phase `the_view_is_the_grant_plus_the_path_to_it`.
        // The view is the granted subtree plus the directories leading to
        // it, nothing else — ancestors must exist or the grant is
        // unreachable; siblings must not, or denial becomes an absence.
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let cap = CapOverride::new(
                Subject::User(Ed25519PublicKey(ALICE)),
                Scope::FsPath("/w/docs".into()),
                Effect::Grant,
                CapAccess::Read,
            );
            let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(0));

            for present in ["/", "/w", "/w/docs", "/w/docs/a", "/w/docs/a/b.txt"] {
                assert!(
                    acl.visible(&ALICE, present),
                    "{present} must be part of the view"
                );
            }
            for absent in [
                "/w/other",
                "/w/other/private.md",
                "/w/docsSECRET",
                "/elsewhere",
            ] {
                assert!(
                    !acl.visible(&ALICE, absent),
                    "{absent} must be absent from the view, not merely denied"
                );
            }
        }
        // Phase `an_identity_with_no_grant_sees_an_empty_root`.
        // An identity holding nothing sees an empty root, not a broken
        // mount. "The folder is empty" is a state every program handles.
        {
            let acl = WorkspaceAcl::new();
            assert!(acl.visible(&ALICE, "/"), "the mount root always exists");
            assert!(!acl.visible(&ALICE, "/anything"));
            assert!(!acl.visible(&ALICE, "/w/docs"));
        }
        // Phase `a_restriction_narrows_writing_without_hiding_anything`.
        // A `Restrict` narrows what may be done, never what may be seen:
        // every verb implies `Read`, so a restricted subtree stays visible
        // and becomes read-only — safe to write into, since nothing is hidden.
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let key = Ed25519PublicKey(ALICE);
            let caps = vec![
                CapOverride::new(
                    Subject::User(key),
                    Scope::FsPath("/w".into()),
                    Effect::Grant,
                    CapAccess::Write,
                ),
                CapOverride::new(
                    Subject::User(key),
                    Scope::FsPath("/w/readonly".into()),
                    Effect::Restrict,
                    CapAccess::Read,
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
        // Phase `an_expired_grant_leaves_no_visible_ancestors`.
        // An expired grant nominates nothing: the ancestor test re-runs the
        // real decision, so expiry removes the trail as well as the target.
        {
            use collab_types::{Access as CapAccess, Effect, Expiry, Scope, Subject};

            let cap = CapOverride::new(
                Subject::User(Ed25519PublicKey(ALICE)),
                Scope::FsPath("/w/docs".into()),
                Effect::Grant,
                CapAccess::Read,
            )
            .expiring(Expiry::AtUnix(500));

            let live =
                WorkspaceAcl::from_capabilities(ALICE, std::slice::from_ref(&cap), &[], Some(499));
            assert!(live.visible(&ALICE, "/w"), "while live, the trail exists");

            let lapsed = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(501));
            assert!(!lapsed.visible(&ALICE, "/w/docs"));
            assert!(!lapsed.visible(&ALICE, "/w"));
        }
        // Phase `a_write_grant_is_visible_and_so_are_its_ancestors`.
        // Write-only-looking grants still make the subtree visible: `Write`
        // implies `Read`, so a writable subtree is a readable one and its
        // ancestors must exist.
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let cap = CapOverride::new(
                Subject::User(Ed25519PublicKey(ALICE)),
                Scope::FsPath("/w/inbox".into()),
                Effect::Grant,
                CapAccess::Write,
            );
            let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(0));
            assert!(acl.visible(&ALICE, "/w"));
            assert!(acl.visible(&ALICE, "/w/inbox/note.md"));
            assert!(!acl.visible(&ALICE, "/w/outbox"));
        }
        // Phase `direct_entries_produce_visible_ancestors_too`.
        // Direct entries take the same route as capabilities, so a
        // capability-free grant also produces walkable ancestors.
        {
            let mut acl = WorkspaceAcl::new();
            acl.grant_read(ALICE, "/w/docs");
            assert!(acl.visible(&ALICE, "/w"));
            assert!(acl.visible(&ALICE, "/w/docs/report.md"));
            assert!(!acl.visible(&ALICE, "/w/other"));
            assert!(!acl.visible(&BOB, "/w"));
        }
    }

    #[test]
    fn peers_are_decided_only_by_the_workspace_capability_list() {
        // Phase `a_peer_is_decided_by_the_workspaces_own_capability_list`.
        // A capability list written for Bob decides for Bob, even on a mount
        // bound as Alice — `permits` would answer `false` for every peer,
        // correct for FUSE, useless for admitting a connection.
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let cap = CapOverride::new(
                Subject::User(Ed25519PublicKey(BOB)),
                Scope::FsPath("/docs".into()),
                Effect::Grant,
                CapAccess::Read,
            );
            let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(0));
            assert!(acl.permits_peer(&BOB, "/docs/report.md", Access::Read));
            assert!(acl.reads_anything(&BOB));
            assert!(!acl.permits_peer(&BOB, "/docs/report.md", Access::Write));
            assert!(!acl.permits_peer(&BOB, "/secrets", Access::Read));
        }
        // Phase `an_unnamed_peer_reads_nothing`.
        // An identity the list never names reads nothing — which is what the sync
        // and content planes turn into "served nothing".
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let cap = CapOverride::new(
                Subject::User(Ed25519PublicKey(ALICE)),
                Scope::FsPath("/".into()),
                Effect::Grant,
                CapAccess::Read,
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
        // Phase `revoking_a_peers_grant_stops_it_reading`.
        // Revocation reaches the network plane because the plane asks the
        // live handle, not a snapshot from mount time — the swap
        // `CapabilityFile::refresh` performs when the capability file empties.
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let cap = CapOverride::new(
                Subject::User(Ed25519PublicKey(BOB)),
                Scope::FsPath("/docs".into()),
                Effect::Grant,
                CapAccess::Read,
            );
            let handle =
                AclHandle::new(WorkspaceAcl::from_capabilities(ALICE, &[cap], &[], Some(0)));
            assert!(handle.reads_anything(&BOB));
            assert!(handle.permits_peer(&BOB, "/docs/report.md", Access::Read));

            handle.replace(WorkspaceAcl::new());
            assert!(!handle.reads_anything(&BOB));
            assert!(!handle.permits_peer(&BOB, "/docs/report.md", Access::Read));
        }
        // Phase `a_peer_does_not_inherit_the_mounts_principals`.
        // A capability granted to a room the mount is in belongs to the
        // mount. Lending it to a dialer would hand every caller the host's
        // authority, so a peer carries only its own key.
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let room = Principal::Room("6b3f9a10-0000-4000-8000-00000000000a".to_string());
            let cap = CapOverride::new(
                Subject::Principal(room.clone()),
                Scope::FsPath("/".into()),
                Effect::Grant,
                CapAccess::Read,
            );
            let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[room], Some(0));
            assert!(
                acl.permits(&ALICE, "/x", Access::Read),
                "the mount holds it"
            );
            assert!(!acl.reads_anything(&BOB), "a caller does not");
            assert!(!acl.permits_peer(&BOB, "/x", Access::Read));
        }
    }
}
