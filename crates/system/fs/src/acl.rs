//! Per-identity workspace ACL, the filesystem enforcement point: every open, read and write is
//! checked against the workspace ACL keyed on identity (a 32-byte public key), not Unix mode bits,
//! which stay a compatibility veneer.
//!
//! Fail closed: an identity has access only if some entry explicitly grants it, and a mount with no
//! ACL attached keeps Unix-veneer-only behaviour. The one permission vocabulary is the capability
//! model: [`WorkspaceAcl::from_capabilities`] delegates every decision to
//! [`collab_types::decide_capability`], so containment, expiry and deny-bias never drift from the
//! other enforcement points. [`AclEntry`] remains for direct, capability-free grants.
//!
//! A denial is an absence: [`WorkspaceAcl::visible`] decides whether a path is in the identity's view
//! at all, and [`crate::QuickFS`] omits what is not and answers `ENOENT`, a state every program handles.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use collab_types::{
    Access as CapAccess, Actor, CapDecision, CapOverride, Ed25519PublicKey, Effect, Principal,
    ResourceRef, Scope, decide_capability, path_covers,
};

/// A Tilo identity: the 32-byte Ed25519 public key that anchors a person or one of their devices.
pub type IdentityKey = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    /// `open` for writing, `write`, `create`, `truncate`, unlink.
    Write,
}

/// One grant: an identity may read and/or write a path subtree. `path_prefix` matches the path
/// itself and everything beneath it, so `"/docs"` matches `/docs` and `/docs/**` but not
/// `/docsxyz`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AclEntry {
    pub identity: IdentityKey,
    pub path_prefix: String,
    pub read: bool,
    pub write: bool,
}

/// The capability list bound to one mount identity: the actor the mount acts as and the
/// capabilities authoritative for it. Decisions are not precomputed into [`AclEntry`]s, since a
/// flat prefix list cannot express a `Restrict` deeper than a `Grant`; the list stays whole and
/// [`collab_types::decide_capability`] is asked about the concrete path at check time.
#[derive(Clone, Debug)]
struct CapBinding {
    identity: IdentityKey,
    actor: Actor,
    /// The complete capabilities with Files mount paths projected into this namespace: grants,
    /// restrictions, subjects and expiries stay intact.
    caps: Vec<CapOverride>,
    /// The instant capability expiry is evaluated at. `None` reads the system clock every check, so
    /// an [`collab_types::Expiry::AtUnix`] expires mid-mount; `Some(t)` pins every check for tests.
    /// Distinct from `decide_capability`'s `now: None`, which skips expiry and would make every
    /// expired capability immortal.
    fixed_now: Option<u64>,
    /// The same instant in a cell a test's [`AclHandle::checking_at`] moves, so it can lapse a grant
    /// while an admission lease holds the policy read.
    #[cfg(test)]
    moving_now: Option<FixedNow>,
}

impl CapBinding {
    fn now(&self) -> u64 {
        #[cfg(test)]
        if let Some(moving) = &self.moving_now {
            return moving.at();
        }
        self.fixed_now.unwrap_or_else(crate::clock::unix_now_secs)
    }

    fn permits(&self, path: &str, access: Access) -> bool {
        self.permits_as(&self.actor, path, access)
    }

    /// [`Self::permits`] asked on behalf of `actor` rather than the actor this binding was created
    /// for: the capability list is the workspace's, so asking about somebody else reads the same
    /// policy.
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
            Some(self.now()),
        ) == CapDecision::Allow
    }

    /// The paths this binding's grants nominate for reading, candidates worth asking
    /// [`Self::permits`] about when deciding whether an ancestor directory has to exist. A
    /// `Restrict` nominates nothing: it only narrows writing.
    fn read_nominations(&self) -> impl Iterator<Item = &str> {
        self.read_nominations_for(&self.actor)
    }

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

/// The normal Permissions surface and application sandbox name Files here; a signed workspace
/// authority binds this mount's policy to its namespace UUID.
const WORKSPACE_FILES_PATH: &str = "/var/workspace";

/// Project only the path coordinate, never the signed policy or grant. Ancestor restrictions map
/// too, so a ceiling on `/var` still restricts the Files subtree at the mount boundary.
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

/// The actor a remote identity is evaluated as: its own person principal and nothing else. The
/// principals a mount proves for itself are statements about this machine, and lending them to a
/// dialer would let any peer inherit the mount's authority.
fn peer_actor(peer: &IdentityKey) -> Actor {
    let key = Ed25519PublicKey(*peer);
    Actor {
        key: Some(key),
        principals: vec![Principal::person_from_key(key)],
    }
}

/// A workspace's per-identity access-control list. Two sources, both
/// A workspace's per-identity access-control list. Two sources, both fail-closed: capability
/// bindings ([`WorkspaceAcl::from_capabilities`], the production path) delegate to
/// [`collab_types::decide_capability`]; direct entries ([`AclEntry`]) are a flat list with union
/// semantics, so absence is denial. An empty ACL grants nothing to anyone.
#[cfg(test)]
#[derive(Clone, Debug)]
struct FixedNow(Arc<std::sync::atomic::AtomicU64>);

#[cfg(test)]
impl FixedNow {
    fn at(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceAcl {
    entries: Vec<AclEntry>,
    bindings: Vec<CapBinding>,
}

impl WorkspaceAcl {
    /// Bind `identity`'s capabilities to this ACL.
    ///
    /// Paths beneath the normal `/var/workspace` projection are evaluated relative to this namespace
    /// root; ancestor scopes map to the root too. The caller's signed list is never modified.
    ///
    /// `caps` is the workspace's whole capability list, unfiltered: capabilities that do not cover
    /// the actor, are not [`collab_types::Scope::FsPath`], or have expired are ignored by the
    /// decision itself, since pre-filtering could drop a `Restrict` that must still bite.
    ///
    /// `subject_principals` are principals the mount can prove besides its own person id. `now` is
    /// seconds since epoch: `None` uses the live clock, `Some(t)` pins every check. A workspace role
    /// never authorises a path: even an owner reaches the filesystem through a capability.
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

    /// Evaluate every capability check at `at`, seconds since epoch, instead of the system clock:
    /// for a test that moves a grant past its expiry between two checks. The cell is shared with
    /// [`AclHandle::checking_at`].
    #[cfg(test)]
    #[must_use]
    pub fn checking_at(mut self, at: Arc<std::sync::atomic::AtomicU64>) -> Self {
        let fixed = FixedNow(at);
        for binding in &mut self.bindings {
            binding.moving_now = Some(fixed.clone());
        }
        self
    }

    /// Bind another identity's capabilities into an existing ACL. Each binding answers only for its
    /// own identity, and a mount serves a single identity, so only one binding is ever consulted
    /// live.
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
            #[cfg(test)]
            moving_now: None,
        });
        self
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, entry: AclEntry) -> &mut Self {
        self.entries.push(entry);
        self
    }

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

    pub fn grant_read(
        &mut self,
        identity: IdentityKey,
        path_prefix: impl Into<String>,
    ) -> &mut Self {
        self.grant(identity, path_prefix, true, false)
    }

    pub fn grant_read_write(
        &mut self,
        identity: IdentityKey,
        path_prefix: impl Into<String>,
    ) -> &mut Self {
        self.grant(identity, path_prefix, true, true)
    }

    pub fn capability_count(&self) -> usize {
        self.bindings.iter().map(|b| b.caps.len()).sum()
    }

    /// The set of identities appearing in the ACL, as a direct grant or a capability binding.
    pub fn identities(&self) -> HashSet<IdentityKey> {
        self.entries
            .iter()
            .map(|e| e.identity)
            .chain(self.bindings.iter().map(|b| b.identity))
            .collect()
    }

    /// Whether anyone but the mount itself may be named here: a direct entry for an identity no
    /// binding answers for, or a capability whose subject is not the bound actor. The shipped list,
    /// which grants the mount's own principal, names nobody else, so only the person's own devices
    /// can be admitted ([`crate::reachability`]).
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

    /// Whether `identity` holds `access` on `path`. Fail-closed: `true` only if a direct entry whose
    /// prefix covers `path` grants it, or a bound capability decides `Allow`. Both routes use
    /// [`collab_types::path_covers`], the function the capability model matches `FsPath` with.
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

    /// Whether the remote identity `peer` holds `access` on `path`: the network counterpart of
    /// [`Self::permits`], which is `false` for every peer. Same list, same `decide_capability`, same
    /// deny-bias; only the actor differs.
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

    /// Whether `peer` holds a read grant on anything here: the admission question, the coarsest one,
    /// answerable before knowing which path a request concerns. Only live grants make it `true`; a
    /// `Restrict` never removes reading.
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

    /// Whether `path` is part of the view `identity` sees: it may read the path, or the path is an
    /// ancestor of one it may read. [`crate::QuickFS`] turns `false` into `ENOENT` and an omitted
    /// entry. The mount root is always visible, so an identity holding nothing sees an empty
    /// directory.
    ///
    /// Ancestry is decided against the granted paths, each re-checked through [`Self::permits`], so
    /// an expired grant removes its ancestors' visibility too.
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

/// The ACL a live mount enforces, replaceable while the mount is up. Holding a [`WorkspaceAcl`]
/// snapshot would fix the policy at mount time, so a capability granted or revoked later would not
/// reach the filesystem until remount; the mount holds a handle and the policy's owner swaps the
/// contents under it.
#[derive(Clone, Debug)]
pub struct AclHandle {
    acl: Arc<RwLock<WorkspaceAcl>>,
    #[cfg(test)]
    check_at: Option<FixedNow>,
    revision: Arc<std::sync::atomic::AtomicU64>,
    revision_watch: tokio::sync::watch::Sender<u64>,
}

impl AclHandle {
    pub(crate) fn admission_lease(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, WorkspaceAcl>, ()> {
        self.acl.read().map_err(|_| ())
    }

    pub fn new(acl: WorkspaceAcl) -> Self {
        let (revision_watch, _) = tokio::sync::watch::channel(0_u64);
        Self {
            #[cfg(test)]
            check_at: acl.bindings.iter().find_map(|b| b.moving_now.clone()),
            acl: Arc::new(RwLock::new(acl)),
            revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            revision_watch,
        }
    }

    /// Evaluate every capability check at `at`, seconds since epoch, from now on. It moves the cell
    /// [`WorkspaceAcl::checking_at`] shares with the bindings, so it answers while an admission lease
    /// holds the policy read.
    #[cfg(test)]
    pub fn checking_at(&self, at: u64) {
        if let Some(fixed) = &self.check_at {
            fixed.0.store(at, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Whether `identity` holds `access` on `path`, per the ACL bound right now; no second decision,
    /// only a load.
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

    /// Whether `peer` reads anything here, per the ACL bound right now. Read live rather than at
    /// connect time, so a revocation reaches the network plane on the next request.
    pub fn reads_anything(&self, peer: &IdentityKey) -> bool {
        match self.acl.read() {
            Ok(acl) => acl.reads_anything(peer),
            Err(_) => false,
        }
    }

    /// Whether `path` is part of `identity`'s view, per the ACL bound right now. Read through the
    /// same live handle, so a revocation cannot stop permitting a path while still listing it.
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

    pub fn revision(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Subscribe to policy replacements without polling the filesystem. The latest generation is
    /// enough because derived ACL views coalesce intermediate replacements before rebuilding.
    pub fn subscribe_revision(&self) -> tokio::sync::watch::Receiver<u64> {
        self.revision_watch.subscribe()
    }

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
        {
            let acl = WorkspaceAcl::new();
            assert!(!acl.permits(&ALICE, "/anything", Access::Read));
            assert!(!acl.permits(&ALICE, "/", Access::Write));
        }
        {
            let mut acl = WorkspaceAcl::new();
            acl.grant_read(ALICE, "/docs");
            assert!(acl.permits(&ALICE, "/docs", Access::Read));
            assert!(acl.permits(&ALICE, "/docs/report.md", Access::Read));
            assert!(!acl.permits(&ALICE, "/docs", Access::Write));
            assert!(!acl.permits(&BOB, "/docs/report.md", Access::Read));
        }
        {
            let mut acl = WorkspaceAcl::new();
            acl.grant_read(ALICE, "/doc");
            assert!(acl.permits(&ALICE, "/doc/a", Access::Read));
            assert!(acl.permits(&ALICE, "/doc", Access::Read));
            assert!(!acl.permits(&ALICE, "/document", Access::Read));
        }
        {
            let mut acl = WorkspaceAcl::new();
            acl.grant_read_write(ALICE, "/");
            assert!(acl.permits(&ALICE, "/x/y/z", Access::Read));
            assert!(acl.permits(&ALICE, "/x/y/z", Access::Write));
        }
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
            assert!(direct.permits(&ALICE, "/w/docs/a/b.txt", Access::Read));
            assert!(!direct.permits(&ALICE, "/w/docsSECRET", Access::Read));
            assert!(capacl.permits(&ALICE, "/w/docs/a/b.txt", Access::Read));
            assert!(!capacl.permits(&ALICE, "/w/docsSECRET", Access::Read));
        }
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
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let cap = CapOverride::new(
                Subject::workspace("team"),
                Scope::FsPath("/w/team".into()),
                Effect::Grant,
                CapAccess::Write,
            );
            let member = WorkspaceAcl::from_capabilities(
                ALICE,
                std::slice::from_ref(&cap),
                &[Principal::Workspace("team".into())],
                Some(0),
            );
            let outsider =
                WorkspaceAcl::from_capabilities(BOB, std::slice::from_ref(&cap), &[], Some(0));

            assert!(member.permits(&ALICE, "/w/team/plan.md", Access::Write));
            assert!(!outsider.permits(&BOB, "/w/team/plan.md", Access::Read));
        }
    }

    #[test]
    fn the_visible_view_is_each_grant_plus_its_ancestors() {
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
        {
            let acl = WorkspaceAcl::new();
            assert!(acl.visible(&ALICE, "/"), "the mount root always exists");
            assert!(!acl.visible(&ALICE, "/anything"));
            assert!(!acl.visible(&ALICE, "/w/docs"));
        }
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
            for path in ["/w", "/w/notes.txt", "/w/readonly", "/w/sub/deep.txt"] {
                assert!(
                    acl.visible(&ALICE, path),
                    "{path} is inside the write grant and must be in the view"
                );
            }
        }
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
        {
            use collab_types::{Access as CapAccess, Effect, Scope, Subject};

            let workspace =
                Principal::Workspace("6b3f9a10-0000-4000-8000-00000000000a".to_string());
            let cap = CapOverride::new(
                Subject::Principal(workspace.clone()),
                Scope::FsPath("/".into()),
                Effect::Grant,
                CapAccess::Read,
            );
            let acl = WorkspaceAcl::from_capabilities(ALICE, &[cap], &[workspace], Some(0));
            assert!(
                acl.permits(&ALICE, "/x", Access::Read),
                "the mount holds it"
            );
            assert!(!acl.reads_anything(&BOB), "a caller does not");
            assert!(!acl.permits_peer(&BOB, "/x", Access::Read));
        }
    }
}
