//! The sandbox: what an app's code may touch outside the documents it
//! publishes — filesystem paths, collaborative file records, devices and
//! network hosts — as `(subject × scope × access)` capabilities.
//!
//! Granted to an app at install and through portals, and never what a person
//! grants an agent: who may invoke which action is decided by
//! `tilo-action-policy` over the published tree.
//!
//! [`decide_capability`] is fail-closed and deny-biased: no matching grant
//! denies, and a matching `Restrict` caps access at its own [`Access`] even if
//! a `Grant` would allow more. `Read` is the floor: a `Restrict` makes a scope
//! read-only and never takes reading away. To keep a path, device, host or
//! record from an app, do not grant it.

use serde::{Deserialize, Serialize};
mod fs_authority_v1;
pub mod net;

pub use net::{Allowed, EgressPolicy, HostPattern, NetHostError, NetHostPattern, host_covers};

use crate::crypto::Ed25519PublicKey;

/// How far a sandbox capability reaches into its resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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

/// A named principal — the string-keyed half of the subject space.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Principal {
    /// A human, by stable id. When the person has a signing key the id is its
    /// lowercase hex, so a `Person` principal and a matching [`Subject::User`]
    /// are interchangeable.
    Person(String),
    /// An application, by its stable app id.
    App(String),
    /// An automated agent.
    Agent(String),
    /// A bearer token: whoever presents this token id holds the capability.
    Token(String),
    /// A room (workspace): held by everyone in it.
    Room(String),
}

impl Principal {
    /// Name a person by their Ed25519 public key (canonical lowercase hex),
    /// keeping `Subject::User` and `Principal::Person` interchangeable.
    pub fn person_from_key(key: Ed25519PublicKey) -> Self {
        Principal::Person(hex::encode(key.0))
    }

    /// Is this principal a human?
    pub fn is_person(&self) -> bool {
        matches!(self, Principal::Person(_))
    }

    pub fn id(&self) -> &str {
        match self {
            Principal::Person(s)
            | Principal::App(s)
            | Principal::Agent(s)
            | Principal::Token(s)
            | Principal::Room(s) => s.as_str(),
        }
    }
}

/// Who a capability is about: one signing identity or a named [`Principal`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Subject {
    /// One specific identity, by public key.
    User(Ed25519PublicKey),
    /// A named principal. The general case.
    Principal(Principal),
}

impl Subject {
    /// Does this subject cover `actor`?
    pub fn matches_actor(&self, actor: &Actor) -> bool {
        match self {
            Subject::User(pk) => {
                actor.key == Some(*pk)
                    || actor.principals.contains(&Principal::person_from_key(*pk))
            }
            Subject::Principal(p) => {
                if let (Principal::Person(_), Some(k)) = (p, actor.key)
                    && *p == Principal::person_from_key(k)
                {
                    return true;
                }
                actor.principals.contains(p)
            }
        }
    }

    pub fn app(app_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::App(app_id.into()))
    }

    pub fn person(user_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::Person(user_id.into()))
    }

    pub fn agent(agent_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::Agent(agent_id.into()))
    }

    pub fn token(token_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::Token(token_id.into()))
    }

    pub fn room(room_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::Room(room_id.into()))
    }

    /// Is this subject a person?
    pub fn is_person(&self) -> bool {
        match self {
            Subject::User(_) => true,
            Subject::Principal(p) => p.is_person(),
        }
    }
}

/// The thing asking to reach a sandboxed resource: every [`Principal`] it can
/// prove it is, and its signing key if it has one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Actor {
    /// The actor's signing key, if it has one.
    pub key: Option<Ed25519PublicKey>,
    /// Every principal this actor presents.
    pub principals: Vec<Principal>,
}

impl Actor {
    /// A person identified by signing key.
    pub fn signed(key: Ed25519PublicKey) -> Self {
        Self {
            key: Some(key),
            principals: vec![Principal::person_from_key(key)],
        }
    }

    /// A person identified by a stable string id.
    pub fn person(user_id: impl Into<String>) -> Self {
        Self {
            key: None,
            principals: vec![Principal::Person(user_id.into())],
        }
    }

    /// An application.
    pub fn app(app_id: impl Into<String>) -> Self {
        Self {
            key: None,
            principals: vec![Principal::App(app_id.into())],
        }
    }

    /// An automated agent.
    pub fn agent(agent_id: impl Into<String>) -> Self {
        Self {
            key: None,
            principals: vec![Principal::Agent(agent_id.into())],
        }
    }

    /// Add a principal (a room the actor is in, a token it presented,
    /// the app it is running as).
    #[must_use]
    pub fn with_principal(mut self, p: Principal) -> Self {
        if !self.principals.contains(&p) {
            self.principals.push(p);
        }
        self
    }

    /// Add room membership.
    #[must_use]
    pub fn in_room(self, room: impl Into<String>) -> Self {
        self.with_principal(Principal::Room(room.into()))
    }

    /// The person this actor is, if it presents one.
    pub fn person_id(&self) -> Option<&str> {
        self.principals.iter().find_map(|p| match p {
            Principal::Person(id) => Some(id.as_str()),
            _ => None,
        })
    }
}

/// What a sandbox capability ranges over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
/// rooms: the sandbox capabilities people hold over shared files, and who
/// may reach each room a relay carries (keyed by the relay's room id) — the
/// room's members, as its policy names them.
///
/// Written as an object. It also reads the bare capability array that
/// preceded it ([`fs_authority_v1`]): that file persists on a machine across
/// image updates, and failing to read it leaves the filesystem unmounted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FsAuthority {
    pub capabilities: Vec<CapOverride>,
    pub room_members: std::collections::BTreeMap<String, Vec<RoomMember>>,
}

/// The current, object form of [`FsAuthority`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FsAuthorityV2 {
    #[serde(default)]
    capabilities: Vec<CapOverride>,
    #[serde(default)]
    room_members: std::collections::BTreeMap<String, Vec<RoomMember>>,
}

impl<'de> Deserialize<'de> for FsAuthority {
    /// An object is the current form; an array is the list before it. The
    /// shape alone decides, so an error names what is wrong inside the form
    /// the file is actually in.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};

        struct Either;
        impl<'de> serde::de::Visitor<'de> for Either {
            type Value = FsAuthority;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "a file authority object (`{\"capabilities\": [...]}`) or the capability \
                     array written before it",
                )
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<FsAuthority, A::Error> {
                let FsAuthorityV2 {
                    capabilities,
                    room_members,
                } = FsAuthorityV2::deserialize(MapAccessDeserializer::new(map))?;
                Ok(FsAuthority {
                    capabilities,
                    room_members,
                })
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> Result<FsAuthority, A::Error> {
                let old = Vec::deserialize(SeqAccessDeserializer::new(seq))?;
                Ok(FsAuthority::from(fs_authority_v1::migrate(old)))
            }
        }

        deserializer.deserialize_any(Either)
    }
}

/// A person a relay admits to one room: until `until` (Unix seconds,
/// exclusive) when their membership lapses, or for as long as they stay a
/// member.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomMember {
    pub user: crate::crypto::Ed25519PublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
}

impl RoomMember {
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
            room_members: std::collections::BTreeMap::new(),
        }
    }
}

/// What a launch hands the apps of a room: which of them author the room
/// (the room's grants let them change it; see
/// `tilo_action_policy::roles::app_authorship`), and the room's sandbox
/// capabilities. An app that is not an author opens as a viewer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchGrants {
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub sandbox: Vec<CapOverride>,
}

impl LaunchGrants {
    /// What reaches the app `app` itself: whether it authors, and only the
    /// capabilities naming it. Restrictions travel too: the launcher's
    /// decision needs them to narrow a grant.
    #[must_use]
    pub fn for_app(&self, app: &str) -> Self {
        let want = Subject::app(app);
        Self {
            authors: self
                .authors
                .iter()
                .filter(|author| author.as_str() == app)
                .cloned()
                .collect(),
            sandbox: self
                .sandbox
                .iter()
                .filter(|capability| capability.subject == want)
                .cloned()
                .collect(),
        }
    }

    /// Whether `app` authors the room.
    #[must_use]
    pub fn authors(&self, app: &str) -> bool {
        self.authors.iter().any(|author| author == app)
    }

    /// Nothing to hand over.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.authors.is_empty() && self.sandbox.is_empty()
    }
}

/// Does the path-prefix `prefix` cover `path`, respecting component/// Does the path-prefix `prefix` cover `path`, respecting component
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

/// A sandboxed resource an app might reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceRef {
    FsPath(String),
    Device(String),
    NetHost(String),
    FsRecord(String),
}

/// When a capability dies — the `expiry` of `(subject, resource, verbs,
/// expiry)`.
///
/// The whole lifetime vocabulary: the portal's "Just once" / "Until I log
/// out" / "Always" map to [`Expiry::AfterUse`], `EndOfSession` and
/// `Never` respectively, not a parallel enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Effect {
    /// Extend a subject's access within the scope.
    Grant,
    /// Cap a subject's access within the scope at the capability's
    /// [`Access`]: `Restrict { Read }` means read-only. A restriction never
    /// denies [`Access::Read`].
    Restrict,
}

/// One sandbox capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapOverride {
    pub subject: Subject,
    pub scope: Scope,
    pub effect: Effect,
    pub access: Access,
    /// Stable id: revocation and the replicated map key on it.
    pub id: String,
    pub expiry: Expiry,
    /// Who granted it, for audit and the share list.
    pub granted_by: Option<Principal>,
}

impl CapOverride {
    pub fn new(subject: Subject, scope: Scope, effect: Effect, access: Access) -> Self {
        Self {
            subject,
            scope,
            effect,
            access,
            id: String::new(),
            expiry: Expiry::Never,
            granted_by: None,
        }
    }

    pub fn grant(subject: Subject, scope: Scope, access: Access) -> Self {
        Self::new(subject, scope, Effect::Grant, access)
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
    pub fn granted_by(mut self, who: Principal) -> Self {
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

/// May `actor` reach `resource` with `access`? A live matching grant whose
/// access implies the request allows, unless a live matching restriction caps
/// it below the request. `now` in Unix seconds; `None` skips expiry.
///
/// Reading is refused only when no live grant covers the resource: every
/// restriction's access implies `Read`, so none caps below it.
pub fn decide_capability(
    actor: &Actor,
    resource: &ResourceRef,
    access: Access,
    caps: &[CapOverride],
    now: Option<u64>,
) -> CapDecision {
    let mut granted = false;
    let mut restricted = false;
    for cap in caps {
        if !cap.is_live_at(now)
            || !cap.subject.matches_actor(actor)
            || !cap.scope.matches_resource(resource)
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_reaches_its_scope_and_a_restriction_caps_it() {
        let actor = Actor::app("so.tilo.books");
        let mut caps = vec![CapOverride::grant(
            Subject::app("so.tilo.books"),
            Scope::FsPath("/approved".into()),
            Access::Write,
        )];
        let book = ResourceRef::FsPath("/approved/book.txt".into());
        assert_eq!(
            decide_capability(&actor, &book, Access::Write, &caps, None),
            CapDecision::Allow
        );
        let sibling = ResourceRef::FsPath("/approvedX".into());
        assert_eq!(
            decide_capability(&actor, &sibling, Access::Read, &caps, None),
            CapDecision::NotGranted
        );
        for other in [
            ResourceRef::Device("camera".into()),
            ResourceRef::NetHost("example.com".into()),
            ResourceRef::FsRecord("file-1".into()),
        ] {
            assert_eq!(
                decide_capability(&actor, &other, Access::Read, &caps, None),
                CapDecision::NotGranted
            );
        }
        caps.push(CapOverride::new(
            Subject::app("so.tilo.books"),
            Scope::FsPath("/".into()),
            Effect::Restrict,
            Access::Read,
        ));
        assert_eq!(
            decide_capability(&actor, &book, Access::Write, &caps, None),
            CapDecision::DeniedByRestriction
        );
        assert_eq!(
            decide_capability(&actor, &book, Access::Read, &caps, None),
            CapDecision::Allow
        );
        let stranger = Actor::app("dev.example.other");
        assert_eq!(
            decide_capability(&stranger, &book, Access::Read, &caps, None),
            CapDecision::NotGranted
        );
    }

    #[test]
    fn a_restriction_never_removes_read_at_any_scope() {
        let actor = Actor::app("so.tilo.books");
        let cases = [
            (
                Scope::FsPath("/w".into()),
                ResourceRef::FsPath("/w/a.txt".into()),
            ),
            (
                Scope::Device("camera".into()),
                ResourceRef::Device("camera".into()),
            ),
            (
                Scope::NetHost("example.com".into()),
                ResourceRef::NetHost("example.com".into()),
            ),
            (
                Scope::FsRecord(EVERY.into()),
                ResourceRef::FsRecord("file-1".into()),
            ),
        ];
        for (scope, resource) in cases {
            for granted in [Access::Read, Access::Write] {
                for capped in [Access::Read, Access::Write] {
                    let caps = [
                        CapOverride::grant(Subject::app("so.tilo.books"), scope.clone(), granted),
                        CapOverride::new(
                            Subject::app("so.tilo.books"),
                            scope.clone(),
                            Effect::Restrict,
                            capped,
                        ),
                    ];
                    assert_eq!(
                        decide_capability(&actor, &resource, Access::Read, &caps, Some(0)),
                        CapDecision::Allow,
                        "{scope:?}: grant {granted:?}, restrict {capped:?}"
                    );
                }
            }
            // The restriction matched: it is why Write is refused, not a
            // scope that missed.
            let caps = [
                CapOverride::grant(Subject::app("so.tilo.books"), scope.clone(), Access::Write),
                CapOverride::new(
                    Subject::app("so.tilo.books"),
                    scope.clone(),
                    Effect::Restrict,
                    Access::Read,
                ),
            ];
            assert_eq!(
                decide_capability(&actor, &resource, Access::Write, &caps, Some(0)),
                CapDecision::DeniedByRestriction,
                "{scope:?}"
            );
            // Only the absence of a grant refuses reading.
            assert_eq!(
                decide_capability(&actor, &resource, Access::Read, &caps[1..], Some(0)),
                CapDecision::NotGranted,
                "{scope:?}"
            );
        }
    }

    #[test]
    fn write_implies_read_and_not_the_other_way() {
        assert!(Access::Write.implies(Access::Read));
        assert!(Access::Write.implies(Access::Write));
        assert!(Access::Read.implies(Access::Read));
        assert!(!Access::Read.implies(Access::Write));
    }
}
