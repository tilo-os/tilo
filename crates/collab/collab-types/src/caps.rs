//! The sandbox: what code may reach outside the documents it publishes —
//! filesystem paths, collaborative file records, devices and network hosts — as
//! `(who × scope × access)` capabilities.
//!
//! A capability names a [`Grantee`], a member or a role, and names an app when it
//! is that app's sandbox. Granted at install and through portals, and never what a
//! person grants a member: who may invoke which action is decided by
//! `tilo-action-policy` over the published tree.
//!
//! [`decide_capability`] is fail-closed and deny-biased: no matching grant
//! denies, and a matching `Restrict` caps access at its own [`Access`] even if
//! a `Grant` would allow more. `Read` is the floor: a `Restrict` makes a scope
//! read-only and never takes reading away. To keep a path, device, host or
//! record from an app, do not grant it.

use serde::{Deserialize, Serialize};
pub mod net;

pub use net::{Allowed, EgressPolicy, HostPattern, NetHostError, NetHostPattern, host_covers};
use tilo_action_policy::{Acting, Grantee, MemberId};

/// How far a sandbox capability reaches into its resource.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    tilo_resource::Model,
)]
#[serde(deny_unknown_fields)]
pub enum Access {
    /// Read, observe, capture. The floor: `Write` implies it, so no
    /// [`Effect::Restrict`] removes it. Only the absence of a grant does.
    Read,
    /// Change, send, drive. Implies `Read`.
    Write,
}

impl Access {
    /// Does holding `self` reach `other`?
    pub fn implies(self, other: Access) -> bool {
        self >= other
    }
}

/// What a sandbox capability ranges over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub enum Scope {
    /// A filesystem path and everything below it.
    FsPath(String),
    /// A device class (`camera`, `microphone`, `printer`, ...), or `*` for
    /// every device.
    Device(String),
    /// A network host pattern; see [`net`].
    NetHost(String),
    /// A stable collaborative-file record id, or `*` for every record.
    FsRecord(String),
}

/// The scope pattern a [`Scope::Device`] or [`Scope::FsRecord`] uses for
/// every one.
pub const EVERY: &str = "*";

impl Scope {
    /// Does this scope cover `resource`?
    pub fn matches_resource(&self, resource: &ResourceRef) -> bool {
        match (self, resource) {
            (Scope::FsPath(prefix), ResourceRef::FsPath(path)) => path_covers(prefix, path),
            (Scope::Device(d), ResourceRef::Device(e))
            | (Scope::FsRecord(d), ResourceRef::FsRecord(e)) => d == EVERY || d == e,
            (Scope::NetHost(h), ResourceRef::NetHost(target)) => host_covers(h, target),
            (Scope::FsPath(_) | Scope::Device(_) | Scope::NetHost(_) | Scope::FsRecord(_), _) => {
                false
            }
        }
    }
}

/// What a governor exports to its filesystem and the relays that carry its
/// workspaces: the sandbox capabilities people hold over shared files, and who
/// may reach each workspace a relay carries (keyed by the relay's workspace
/// id) — the workspace's members, as its policy names them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsAuthority {
    pub capabilities: Vec<CapOverride>,
    /// Who may reach each workspace a relay carries. Absent when the file is a mount's own list.
    #[serde(default)]
    pub workspace_members: std::collections::BTreeMap<String, Vec<WorkspaceMember>>,
}

/// A person a relay admits to one workspace: until `until` (Unix seconds,
/// exclusive) when their membership lapses, or for as long as they stay a
/// member.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceMember {
    pub user: crate::crypto::Ed25519PublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
}

impl WorkspaceMember {
    /// Whether the membership holds at `now` (Unix seconds).
    #[must_use]
    pub fn is_live_at(&self, now: u64) -> bool {
        self.until.is_none_or(|until| now < until)
    }
}

impl From<Vec<CapOverride>> for FsAuthority {
    fn from(capabilities: Vec<CapOverride>) -> Self {
        Self {
            capabilities,
            workspace_members: std::collections::BTreeMap::new(),
        }
    }
}

/// What a launch hands an app: whether the person launching it may change the
/// workspace through it, and the app's sandbox capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchGrants {
    /// Whether the app may change the workspace: its person may.
    #[serde(default)]
    pub author: bool,
    #[serde(default)]
    pub sandbox: Vec<CapOverride>,
}

impl LaunchGrants {
    /// What reaches the app `app` itself: only the capabilities naming it.
    /// Restrictions travel too: the launcher's decision needs them to narrow
    /// a grant.
    #[must_use]
    pub fn for_app(&self, app: &str) -> Self {
        Self {
            author: self.author,
            sandbox: self
                .sandbox
                .iter()
                .filter(|cap| cap.names_app(app))
                .cloned()
                .collect(),
        }
    }

    /// Nothing to hand over.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.author && self.sandbox.is_empty()
    }
}

/// Does the path-prefix `prefix` cover `path`, respecting component
/// boundaries?
///
/// `/w/docs` covers `/w/docs/a.txt` but not `/w/docsX` — a prefix that
/// stops mid-component is not a parent directory. Same rule
/// `fs::acl::prefix_covers` implements; the two must agree byte-for-byte
/// or a path could be readable through one and not the other.
pub fn path_covers(prefix: &str, path: &str) -> bool {
    if prefix == "/" || prefix.is_empty() {
        return true;
    }
    let prefix = prefix.strip_suffix('/').unwrap_or(prefix);
    if path == prefix {
        return true;
    }
    let mut with_sep = String::with_capacity(prefix.len().saturating_add(1));
    with_sep.push_str(prefix);
    with_sep.push('/');
    path.starts_with(&with_sep)
}

/// A sandboxed resource code might reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceRef {
    FsPath(String),
    Device(String),
    NetHost(String),
    FsRecord(String),
}

/// When a capability dies — the `expiry` of `(who, resource, access, expiry)`.
///
/// The whole lifetime vocabulary: the portal's "Just once" / "Until I log
/// out" / "Always" map to [`Expiry::AfterUse`], `EndOfSession` and
/// `Never` respectively, not a parallel enum.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model,
)]
#[serde(deny_unknown_fields)]
pub enum Expiry {
    /// Lives until explicitly revoked, and survives a restart.
    #[default]
    Never,
    /// Dies at this wall-clock instant (seconds since epoch); survives a
    /// restart if not yet past.
    AtUnix(u64),
    /// Dies when the granting session ends. Never written to disk.
    EndOfSession,
    /// Dies as soon as the requesting prompt completes. Never written to disk.
    AfterUse,
}

impl Expiry {
    /// Is a capability with this expiry still live at `now`?
    ///
    /// `now: None` skips time evaluation — used by the wire validator, which
    /// shares no clock with an op's author. `EndOfSession`/`AfterUse` are
    /// live here; their deaths are events (not persisting, dropping after
    /// use), not instants.
    pub fn is_live_at(self, now: Option<u64>) -> bool {
        match (self, now) {
            (Expiry::AtUnix(deadline), Some(now)) => now < deadline,
            _ => true,
        }
    }

    /// Should a capability with this expiry be written to disk?
    ///
    /// The single answer to "does `Always` survive a restart?" — session- and
    /// use-scoped capabilities are excluded so persisting them can't silently
    /// turn a one-shot grant into a permanent one.
    pub fn persists_across_restart(self) -> bool {
        matches!(self, Expiry::Never | Expiry::AtUnix(_))
    }

    /// Does a capability with this expiry outlive one with `other`?
    ///
    /// Order: `Never` > `AtUnix(later)` > `AtUnix(earlier)` > `EndOfSession` >
    /// `AfterUse`. An explicit deadline outranks the session because it
    /// survives a restart.
    pub fn outlives(self, other: Expiry) -> bool {
        fn tier(e: Expiry) -> u8 {
            match e {
                Expiry::AfterUse => 0,
                Expiry::EndOfSession => 1,
                Expiry::AtUnix(_) => 2,
                Expiry::Never => 3,
            }
        }
        match (self, other) {
            (Expiry::AtUnix(a), Expiry::AtUnix(b)) => a > b,
            (a, b) => tier(a) > tier(b),
        }
    }

    /// The longer-lived of two expiries — the single definition of "strongest
    /// scope wins", used both merging a re-grant and reporting an existing
    /// grant's strength. Two answers here would eventually disagree, reading
    /// as a permanent grant silently becoming a session one.
    pub fn longest(a: Expiry, b: Expiry) -> Expiry {
        if a.outlives(b) { a } else { b }
    }
}

/// Whether a capability grants or restricts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub enum Effect {
    /// Extend the grantee's access within the scope.
    Grant,
    /// Cap the grantee's access within the scope at the capability's [`Access`]:
    /// `Restrict { Read }` means read-only. A restriction never denies
    /// [`Access::Read`].
    Restrict,
}

/// One sandbox capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct CapOverride {
    pub grantee: Grantee,
    /// Only through this app, when the capability is that app's sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<tilo_app_id::AppId>,
    pub scope: Scope,
    pub effect: Effect,
    pub access: Access,
    /// Stable id: revocation and the replicated map key on it.
    pub id: String,
    pub expiry: Expiry,
    /// Who granted it, for audit and the share list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_by: Option<MemberId>,
}

impl CapOverride {
    pub fn new(grantee: Grantee, scope: Scope, effect: Effect, access: Access) -> Self {
        Self {
            grantee,
            app: None,
            scope,
            effect,
            access,
            id: String::new(),
            expiry: Expiry::Never,
            granted_by: None,
        }
    }

    pub fn grant(grantee: Grantee, scope: Scope, access: Access) -> Self {
        Self::new(grantee, scope, Effect::Grant, access)
    }

    /// The same capability, reaching only through `app`.
    #[must_use]
    pub fn through(mut self, app: impl Into<tilo_app_id::AppId>) -> Self {
        self.app = Some(app.into());
        self
    }

    /// Whether this is `app`'s sandbox capability.
    #[must_use]
    pub fn names_app(&self, app: &str) -> bool {
        self.app.as_ref().is_some_and(|named| named.as_str() == app)
    }

    /// Whether this capability reaches `acting`: its grantee names a node of the acting tree, and
    /// one that names an app reaches it only through that app.
    ///
    /// A role entry reaches nobody here: the workspace's policy is what knows who holds a role, so
    /// the list a mount enforces is written with its members
    /// (`crates/session/session/src/fs_authority.rs`).
    #[must_use]
    pub fn reaches(&self, acting: &Acting) -> bool {
        if let Some(app) = &self.app
            && acting.app.as_ref() != Some(app)
        {
            return false;
        }
        acting
            .nodes()
            .into_iter()
            .any(|node| match node.by.member_id() {
                Some(member) => match &self.grantee {
                    Grantee::Member(named) => named == member,
                    Grantee::Role(_) => false,
                },
                None => false,
            })
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    #[must_use]
    pub fn with_expiry(mut self, unix_seconds: u64) -> Self {
        self.expiry = Expiry::AtUnix(unix_seconds);
        self
    }

    #[must_use]
    pub fn expiring(mut self, expiry: Expiry) -> Self {
        self.expiry = expiry;
        self
    }

    #[must_use]
    pub fn granted_by(mut self, who: MemberId) -> Self {
        self.granted_by = Some(who);
        self
    }

    pub fn is_live_at(&self, now: Option<u64>) -> bool {
        self.expiry.is_live_at(now)
    }

    pub fn persists_across_restart(&self) -> bool {
        self.expiry.persists_across_restart()
    }
}

/// The outcome of a sandbox check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapDecision {
    Allow,
    /// No live capability covers the resource.
    NotGranted,
    /// A restriction caps access below what was asked.
    DeniedByRestriction,
}

/// May `acting` reach `resource` with `access`?
///
/// A live matching grant whose access implies the request allows, unless a live
/// matching restriction caps it below the request. `now` in Unix seconds; `None`
/// skips expiry. Reading is refused only when no live grant covers the
/// resource: every restriction's access implies `Read`, so none caps below it.
pub fn decide_capability(
    acting: &Acting,
    resource: &ResourceRef,
    access: Access,
    caps: &[CapOverride],
    now: Option<u64>,
) -> CapDecision {
    decide(|cap| cap.reaches(acting), resource, access, caps, now)
}

/// May the app `app` reach `resource` with `access`, judged by the list written
/// for its sandbox?
///
/// A launcher, a runtime and a portal ask on the app's behalf, over the list
/// that app's install was granted: naming the app is the whole statement of who
/// the capability is for, so the grantee is not asked about.
pub fn decide_app(
    app: &str,
    resource: &ResourceRef,
    access: Access,
    caps: &[CapOverride],
    now: Option<u64>,
) -> CapDecision {
    decide(|cap| cap.names_app(app), resource, access, caps, now)
}

fn decide(
    reaches: impl Fn(&CapOverride) -> bool,
    resource: &ResourceRef,
    access: Access,
    caps: &[CapOverride],
    now: Option<u64>,
) -> CapDecision {
    let mut granted = false;
    let mut restricted = false;
    for cap in caps {
        if !cap.is_live_at(now) || !reaches(cap) || !cap.scope.matches_resource(resource) {
            continue;
        }
        match cap.effect {
            Effect::Grant if cap.access.implies(access) => granted = true,
            Effect::Restrict if !cap.access.implies(access) => restricted = true,
            _ => {}
        }
    }
    match (granted, restricted) {
        (true, false) => CapDecision::Allow,
        (_, true) if granted => CapDecision::DeniedByRestriction,
        _ => CapDecision::NotGranted,
    }
}
