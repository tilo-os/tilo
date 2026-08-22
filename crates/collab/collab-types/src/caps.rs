//! The capability model — `(subject × scope × verb)`.
//!
//! The static [`crate::role::role_permits`] table answers the coarse
//! question "may a member of this *role* emit this op class?". That is
//! the base layer and stays the authoritative wire decision. This
//! module is the **richer layer stacked on top**: it lets a permission
//! be expressed at finer granularity than "role → op-class", exactly as
//! the coherence architecture (`docs/coherence-architecture.md` §2)
//! describes:
//!
//! > A permission is `(subject: role|user) × (scope: app | resource-type
//! > | resource-instance | fs-path) × (verb: an exported action, or a
//! > coarse cap read/write/invite/admin)`. Roles are named cap-bundles;
//! > users get roles + individual overrides + per-resource shares.
//!
//! Concretely:
//!
//! - A **role** is a named cap-bundle: [`role_caps`] maps each
//!   [`Role`] to the coarse [`Verb`]s it holds. This is coherent with
//!   `role_permits` (see the `caps_agree_with_role_table` test in
//!   `tests/caps.rs`): the role bundle grants exactly the op classes
//!   the table permits.
//! - An **override** ([`CapOverride`]) narrows or extends the base for a
//!   specific `(subject, scope)`. A `Restrict` caps authority within a
//!   scope below what the role would otherwise allow ("in the finance
//!   app, even editors may only comment"); a `Grant` is a per-resource
//!   *share* that extends one subject's authority on one scope ("share
//!   this doc with Bob, comment-only") without changing their role.
//!
//! The decision function is [`decide`]. It is **fail-closed**: an op
//! with no base authority and no matching grant is denied, and a
//! `Denied` role can never be rescued by a grant (it is an explicit
//! block, per the role table's contract). Deny-bias also applies to
//! restrictions: a matching `Restrict` removes authority even if a
//! `Grant` would otherwise allow.
//!
//! Nothing here does I/O or touches the network — same discipline as
//! the rest of `collab-types`.

use serde::{Deserialize, Serialize};

use crate::channel::ArtifactId;
use crate::crypto::Ed25519PublicKey;
use crate::role::{ArtifactKind, OpClass, Role};

/// A coarse capability verb.
///
/// These are the four coarse caps from the coherence doc
/// (`read/write/invite/admin`) plus `Comment`, which sits between
/// `Read` and `Write` because the `Commenter` role is append-only:
/// it may comment but not edit. The variants are **not** a single
/// linear order — `Invite` is a governance-ish capability orthogonal to
/// the `Read < Comment < Write` content ladder — so containment is
/// expressed explicitly by [`Verb::implies`] rather than by `Ord`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Verb {
    /// See the resource. The floor: every other verb implies it.
    Read,
    /// Append a comment or reaction. Implies `Read`.
    Comment,
    /// Mutate content (splice/move/rename/tombstone/restore). Implies
    /// `Comment` + `Read`.
    Write,
    /// Admit a member / mint an invite token for the scope. Implies
    /// `Read` (you can see what you invite into) but is otherwise
    /// independent of the content ladder.
    Invite,
    /// Govern: issue/revoke attestations, compaction, redaction,
    /// settings. The ceiling: implies every other verb.
    Admin,
}

impl Verb {
    /// Does holding `self` confer authority for `other`?
    ///
    /// Containment, not ordering: `Admin` confers everything, `Write`
    /// confers `Comment`/`Read`, `Invite` confers only `Read` besides
    /// itself. Used by [`decide`] so a `Grant`/`Restrict` phrased in a
    /// coarse verb answers a finer required verb correctly.
    pub fn implies(self, other: Verb) -> bool {
        use Verb::*;
        match self {
            Admin => true,
            Write => matches!(other, Write | Comment | Read),
            Invite => matches!(other, Invite | Read),
            Comment => matches!(other, Comment | Read),
            Read => matches!(other, Read),
        }
    }
}

/// A named principal — the string-keyed half of the subject space.
///
/// [`Subject::User`] names a person by signing key, which is the right
/// answer whenever a signature is available. Most enforcement points in
/// the OS have no signature to check: the portal knows a wayland
/// app-id, an agent broker knows an agent name, an invite link carries
/// a bearer token, and a room grant names a room. `Principal` is the
/// one vocabulary for all of those, tagged by *kind* so the UI can
/// never confuse "Bob, who I shared one file with" for "the Ebook app,
/// which asked for a Documents path".
///
/// The kind tag is load-bearing, not decoration. A share list renders
/// [`Principal::Person`] rows; a workspace roster renders room
/// membership. They are the same machinery and must never be the same
/// list — see [`Principal::is_person`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Principal {
    /// A human, by stable id. When the person has a signing key the id
    /// is its lowercase hex (see [`Principal::person_from_key`]) so a
    /// `Person` principal and a [`Subject::User`] naming the same human
    /// are interchangeable.
    Person(String),
    /// An application, by its stable app id (the wayland app-id the
    /// portal sees). This is the subject of the *only* case that
    /// prompts the user: an app reaching outside what it owns.
    App(String),
    /// An automated agent acting for the user. Same shape as an app —
    /// a subject holding a scoped, revocable capability set — but
    /// tagged distinctly so it can be listed and revoked as an agent.
    Agent(String),
    /// A bearer token: whoever presents this token id holds the
    /// capability. Used by invite links and one-shot share URLs.
    Token(String),
    /// A room (workspace). A capability whose subject is a room is held
    /// by everyone currently in that room; a capability whose
    /// *resource* is a room is room membership itself.
    Room(String),
}

impl Principal {
    /// Name a person by their Ed25519 public key, in the canonical
    /// lowercase-hex form. Keeps [`Subject::User`] and
    /// `Subject::Principal(Principal::Person(..))` interchangeable.
    pub fn person_from_key(key: Ed25519PublicKey) -> Self {
        let mut s = String::with_capacity(64);
        for b in key.0.iter() {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
        }
        Principal::Person(s)
    }

    /// Is this principal a human?
    ///
    /// The share-list / roster split keys off this: a capability whose
    /// subject is a person belongs on the *resource's* share list, and
    /// only room membership belongs in the roster.
    pub fn is_person(&self) -> bool {
        matches!(self, Principal::Person(_))
    }

    /// The bare id string, without the kind tag.
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

/// Who a permission is about.
///
/// A permission's subject is either a whole role (the cap-bundle every
/// holder of that role inherits), one specific signing identity, or a
/// named [`Principal`] (person / app / agent / bearer token / room).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Subject {
    /// Everyone holding this role in the channel.
    Role(Role),
    /// One specific identity, by public key.
    User(Ed25519PublicKey),
    /// A named principal. The general case.
    Principal(Principal),
}

impl Subject {
    /// Does this subject cover an actor with role `role` and key
    /// `author`?
    ///
    /// Signature-only form, kept for the wire validator which always
    /// has both. [`Subject::matches_actor`] is the general form.
    pub fn matches(&self, role: Role, author: Ed25519PublicKey) -> bool {
        self.matches_actor(&Actor::signed(role, author))
    }

    /// Does this subject cover `actor`?
    ///
    /// A `Role` subject matches only an actor that *holds* a role — an
    /// app or bearer token carries none and is therefore never covered
    /// by a role-wide grant. That is deliberate: role bundles describe
    /// people in a room, and an app is not a person in a room.
    pub fn matches_actor(&self, actor: &Actor) -> bool {
        match self {
            Subject::Role(r) => actor.role == Some(*r),
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

    /// Convenience constructor for an app subject.
    pub fn app(app_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::App(app_id.into()))
    }

    /// Convenience constructor for a person subject.
    pub fn person(user_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::Person(user_id.into()))
    }

    /// Convenience constructor for an agent subject.
    pub fn agent(agent_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::Agent(agent_id.into()))
    }

    /// Convenience constructor for a bearer-token subject.
    pub fn token(token_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::Token(token_id.into()))
    }

    /// Convenience constructor for a room subject.
    pub fn room(room_id: impl Into<String>) -> Self {
        Subject::Principal(Principal::Room(room_id.into()))
    }
}

/// The thing asking to act.
///
/// An actor presents *at most one* role (people in a room have one;
/// apps, agents and bearer tokens have none), optionally a signing key,
/// and a set of [`Principal`]s it can prove it is — its own person id,
/// its app id, the rooms it is in, any bearer token it presented.
///
/// `role: None` is the fail-closed default and the correct value for
/// every non-person subject: with no role there is no base authority,
/// so such an actor can only ever act through an explicit capability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Actor {
    /// The role this actor holds in the room, if it holds one.
    pub role: Option<Role>,
    /// The actor's signing key, if it has one.
    pub key: Option<Ed25519PublicKey>,
    /// Every principal this actor presents.
    pub principals: Vec<Principal>,
}

impl Actor {
    /// A person in a room, identified by signing key.
    pub fn signed(role: Role, key: Ed25519PublicKey) -> Self {
        Self {
            role: Some(role),
            key: Some(key),
            principals: vec![Principal::person_from_key(key)],
        }
    }

    /// A person in a room, identified by a stable string id (the shape
    /// the compositor's collaborator table uses).
    pub fn person(role: Option<Role>, user_id: impl Into<String>) -> Self {
        Self {
            role,
            key: None,
            principals: vec![Principal::Person(user_id.into())],
        }
    }

    /// An application. Holds no role — apps act only through explicit
    /// capabilities, which is what makes the portal prompt the *only*
    /// user-facing permission dialog.
    pub fn app(app_id: impl Into<String>) -> Self {
        Self {
            role: None,
            key: None,
            principals: vec![Principal::App(app_id.into())],
        }
    }

    /// An automated agent. Same authority shape as an app.
    pub fn agent(agent_id: impl Into<String>) -> Self {
        Self {
            role: None,
            key: None,
            principals: vec![Principal::Agent(agent_id.into())],
        }
    }

    /// Add a principal (a room the actor is in, a token it presented,
    /// the app it is running as).
    pub fn with_principal(mut self, p: Principal) -> Self {
        if !self.principals.contains(&p) {
            self.principals.push(p);
        }
        self
    }

    /// Add room membership.
    pub fn in_room(self, room: impl Into<String>) -> Self {
        self.with_principal(Principal::Room(room.into()))
    }
}

/// What a permission ranges over.
///
/// Ordered from coarsest (`Workspace`) to finest
/// (`ResourceInstance`/`FsPath`). A `FsPath` scope is meaningful only
/// to the filesystem enforcement point; it never matches a CRDT op's
/// [`ResourceScope`], so an fs-path override is inert against ops (and
/// vice-versa) — the two enforcement points read the same vocabulary
/// but only the scopes that apply to them bite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Scope {
    /// The whole workspace / channel. Matches every op.
    Workspace,
    /// A single application, by its stable app id. Matches ops whose
    /// artifact is owned by that app.
    App(String),
    /// Every artifact of a kind.
    ResourceType(ArtifactKind),
    /// One specific artifact.
    ResourceInstance(ArtifactId),
    /// A filesystem path prefix. Filesystem-only; inert against ops.
    ///
    /// Matching is **recursive and component-aligned**: a scope on
    /// `/w/docs` covers `/w/docs` and `/w/docs/a/b.txt` but not
    /// `/w/docsX`. This is the inheritance-down-the-tree the file
    /// permission model needs.
    FsPath(String),
    /// A hardware or system device class the portal brokers:
    /// `"camera"`, `"microphone"`, `"screencast"`. Filesystem- and
    /// op-inert.
    Device(String),
    /// An outbound network host. `"*"` matches every host; a leading
    /// dot (`".example.com"`) matches the domain and its subdomains;
    /// anything else is an exact hostname.
    NetHost(String),
    /// One action an app exports, named `(app, action)`. The finest
    /// scope there is: it authorises exactly one verb-bearing operation
    /// on one app's surface.
    Action { app: String, action: String },
    /// A room (workspace) as a *resource*. A capability whose scope is
    /// a room and whose subject is a person **is** that person's room
    /// membership — the same machinery as any other share, which is
    /// why there is no second sharing system.
    Room(String),
}

impl Scope {
    /// Does this scope apply to an op with the given resource context?
    pub fn matches_op(&self, ctx: &ResourceScope) -> bool {
        self.matches_resource(&ResourceRef::Artifact {
            app: ctx.app.clone(),
            kind: ctx.kind,
            instance: ctx.instance,
        })
    }

    /// Does this scope cover `resource`?
    ///
    /// Scopes **nest**: `Workspace` covers everything, `App(a)` covers
    /// every artifact and action belonging to `a`, and `FsPath(p)`
    /// covers every path under `p`. A scope that is about a different
    /// enforcement point than the resource (an fs-path scope asked
    /// about a CRDT op, a device scope asked about a path) simply does
    /// not match — it is inert, never permissive.
    pub fn matches_resource(&self, resource: &ResourceRef) -> bool {
        match (self, resource) {
            (Scope::Workspace, _) => true,

            (Scope::App(a), ResourceRef::App(b)) => a == b,
            (Scope::App(a), ResourceRef::Artifact { app: Some(b), .. }) => a == b,
            (Scope::App(a), ResourceRef::Action { app: b, .. }) => a == b,
            (Scope::App(_), _) => false,

            (Scope::ResourceType(k), ResourceRef::Artifact { kind, .. }) => k == kind,
            (Scope::ResourceType(_), _) => false,

            (Scope::ResourceInstance(i), ResourceRef::Artifact { instance, .. }) => i == instance,
            (Scope::ResourceInstance(_), _) => false,

            (Scope::FsPath(prefix), ResourceRef::FsPath(path)) => path_covers(prefix, path),
            (Scope::FsPath(_), _) => false,

            (Scope::Device(d), ResourceRef::Device(e)) => d == e,
            (Scope::Device(_), _) => false,

            (Scope::NetHost(h), ResourceRef::NetHost(target)) => host_covers(h, target),
            (Scope::NetHost(_), _) => false,

            (Scope::Action { app, action }, ResourceRef::Action { app: b, action: c }) => {
                app == b && action == c
            }
            (Scope::Action { .. }, _) => false,

            // KNOWN HOLE — a room-scoped capability authorises nothing a
            // person *does* in the room, and does not survive
            // replication. Two separate defects, both live:
            //
            // 1. `Scope::Room` pairs only with `ResourceRef::Room`. Every
            //    chrome action is a `ResourceRef::Action`, so granting
            //    someone `Write` "on the room" leaves them unable to open
            //    a window, edit a note or bring a record into it. The
            //    grant succeeds, replicates, and permits nothing. The
            //    only scope that actually confers ability across a room
            //    is `Scope::Workspace`, which is what a caller wanting
            //    "make this person an editor here" has to use.
            //
            // 2. The key is the **local workspace id** — `grant_capability`
            //    writes `ws_id.raw().to_string()` and `Workspace::role_of`
            //    compares against `self.id.raw().to_string()`. Two peers
            //    in one room have different local ids for their own
            //    workspace, so a replicated room-scoped capability names
            //    an id that means nothing on the receiving machine and
            //    matches nobody. Keying by `RoomId` is what would fix it,
            //    and that is a migration of persisted state rather than a
            //    one-line change.
            //
            // Until both are addressed, room membership is a display
            // concept (it drives `role_of`'s roster label) rather than an
            // enforcement one. Found while proving room replication; not
            // fixed there because it is a change to the permission model.
            (Scope::Room(r), ResourceRef::Room(s)) => r == s,
            (Scope::Room(_), _) => false,
        }
    }
}

/// Does the path-prefix `prefix` cover `path`, respecting component
/// boundaries?
///
/// `/` covers everything. `/w/docs` covers `/w/docs` and
/// `/w/docs/a.txt`, but **not** `/w/docsX` — a prefix that stops
/// mid-component is not a parent directory. This is the same rule
/// `fs::acl::prefix_covers` implements; the two enforcement points must
/// agree byte-for-byte or a path could be readable through one and not
/// the other.
pub fn path_covers(prefix: &str, path: &str) -> bool {
    if prefix == "/" || prefix.is_empty() {
        return true;
    }
    let prefix = prefix.strip_suffix('/').unwrap_or(prefix);
    if path == prefix {
        return true;
    }
    let mut with_sep = String::with_capacity(prefix.len() + 1);
    with_sep.push_str(prefix);
    with_sep.push('/');
    path.starts_with(&with_sep)
}

/// Does the host pattern `pattern` cover `host`?
///
/// `"*"` covers every host. A pattern starting with `.` covers the
/// domain and every subdomain (`.example.com` covers `example.com` and
/// `api.example.com`). Anything else is an exact, case-insensitive
/// match. Deliberately *not* a glob: `*.evil.com` style patterns invite
/// mistakes and this is a security boundary.
pub fn host_covers(pattern: &str, host: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let pattern = pattern.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    if let Some(domain) = pattern.strip_prefix('.') {
        return host == domain || host.ends_with(&pattern);
    }
    pattern == host
}

/// A resource a subject might act on.
///
/// Resources nest — see [`Scope::matches_resource`]. **Showing** a
/// resource is not an action and needs no capability; only *acting* on
/// one is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceRef {
    /// The room/workspace itself.
    Workspace,
    /// An app's own surface, as a whole.
    App(String),
    /// One CRDT artifact.
    Artifact {
        app: Option<String>,
        kind: ArtifactKind,
        instance: ArtifactId,
    },
    /// A filesystem path (absolute, already normalised by the caller).
    FsPath(String),
    /// A brokered device: `"camera"`, `"microphone"`, `"screencast"`.
    Device(String),
    /// An outbound network host.
    NetHost(String),
    /// One exported action on one app.
    Action { app: String, action: String },
    /// A room, as the target of a membership capability.
    Room(String),
}

/// The resource context an op carries, used to match [`Scope`]s.
///
/// Built by the validator from the op's artifact + the channel's
/// artifact→app / artifact→kind maps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceScope {
    /// The app that owns the artifact, if the channel records one.
    pub app: Option<String>,
    /// The artifact's kind.
    pub kind: ArtifactKind,
    /// The artifact's id.
    pub instance: ArtifactId,
}

/// When a capability dies — the `expiry` of
/// `(subject, resource, verbs, expiry)`.
///
/// This is the *whole* lifetime vocabulary. The compositor's portal
/// prompt offers "Just once" / "Until I log out" / "Always"; those are
/// [`Expiry::AfterUse`], [`Expiry::EndOfSession`] and [`Expiry::Never`]
/// respectively, not a parallel enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Expiry {
    /// Lives until explicitly revoked, and survives a restart.
    #[default]
    Never,
    /// Dies at this wall-clock instant (seconds since the Unix epoch).
    /// Survives a restart if the instant has not passed.
    AtUnix(u64),
    /// Dies when the granting session ends. Never written to disk, so a
    /// restart is what ends it.
    EndOfSession,
    /// Dies as soon as the request that prompted it completes. Never
    /// written to disk.
    AfterUse,
}

impl Expiry {
    /// Is a capability with this expiry still live at `now` (seconds
    /// since the Unix epoch)?
    ///
    /// `now: None` skips time evaluation — used by the wire validator,
    /// which shares no clock with an op's author.
    ///
    /// [`Expiry::EndOfSession`] and [`Expiry::AfterUse`] are live here:
    /// their deaths are events, not instants, enforced by not persisting
    /// them and by the caller dropping them after use.
    pub fn is_live_at(self, now: Option<u64>) -> bool {
        match (self, now) {
            (Expiry::AtUnix(deadline), Some(now)) => now < deadline,
            _ => true,
        }
    }

    /// Should a capability with this expiry be written to disk?
    ///
    /// This is the single answer to "does `Always` survive a restart?".
    /// Session- and use-scoped capabilities are deliberately excluded:
    /// persisting them is what would silently turn a one-shot grant into
    /// a permanent one.
    pub fn persists_across_restart(self) -> bool {
        matches!(self, Expiry::Never | Expiry::AtUnix(_))
    }

    /// Does a capability with this expiry outlive one with `other`?
    ///
    /// The order is `Never` > `AtUnix(later)` > `AtUnix(earlier)` >
    /// `EndOfSession` > `AfterUse`. An explicit deadline is treated as
    /// longer-lived than the session because it survives a restart,
    /// which is the property that actually distinguishes them.
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

    /// The longer-lived of two expiries.
    ///
    /// The single definition of "strongest scope wins", used both when
    /// merging a re-grant and when the portal reports the strength of an
    /// existing grant. Two answers to this question would eventually
    /// disagree, and the disagreement would read as a permanent grant
    /// silently becoming a session one.
    pub fn longest(a: Expiry, b: Expiry) -> Expiry {
        if a.outlives(b) { a } else { b }
    }
}

/// Whether an override grants or restricts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Effect {
    /// Extend a subject's authority within the scope (a per-resource
    /// share / individual override upward).
    Grant,
    /// Cap a subject's authority within the scope below the role
    /// baseline (a narrowing).
    Restrict,
}

/// **The capability.** `(subject × scope × verb)`, an expiry, and
/// whether it grants or restricts.
///
/// This one record is the whole primitive. Room membership is a
/// capability whose scope is [`Scope::Room`]; sharing a single file is
/// a capability whose scope is [`Scope::FsPath`]; an app's portal grant
/// is a capability whose subject is [`Principal::App`]. There is no
/// second sharing system and no second vocabulary — anything that wants
/// to express "X may do Y to Z until T" writes one of these.
///
/// A `Grant { verb: Comment }` says "this subject may (at least)
/// comment within this scope". A `Restrict { verb: Comment }` says
/// "this subject may do **no more than** comment within this scope" —
/// any required verb not implied by `Comment` is removed.
///
/// The added fields all carry `#[serde(default)]` so capability records
/// persisted before they existed still load: no id, no expiry (never
/// expires), no recorded grantor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapOverride {
    /// Who the override applies to.
    pub subject: Subject,
    /// What it ranges over.
    pub scope: Scope,
    /// Grant (extend) or Restrict (narrow).
    pub effect: Effect,
    /// The verb the effect is phrased in.
    pub verb: Verb,
    /// Stable id, so a specific capability can be revoked by name even
    /// after it has replicated to peers that never saw it granted.
    /// Empty means "unnamed" — such a capability can only be revoked by
    /// matching on its content.
    #[serde(default)]
    pub id: String,
    /// When this capability dies. An expired capability is invisible to
    /// [`decide_actor`]: it is not "denied by restriction", it simply is
    /// not there.
    #[serde(default)]
    pub expiry: Expiry,
    /// Who granted this, for the share-list UI and for audit. Advisory:
    /// authority to grant is enforced where the grant is *made*, not
    /// here.
    #[serde(default)]
    pub granted_by: Option<Principal>,
}

impl CapOverride {
    /// The general constructor. Prefer the named helpers below when one
    /// fits — they read better at the call site.
    pub fn new(subject: Subject, scope: Scope, effect: Effect, verb: Verb) -> Self {
        Self {
            subject,
            scope,
            effect,
            verb,
            id: String::new(),
            expiry: Expiry::Never,
            granted_by: None,
        }
    }

    /// Convenience: a per-resource share granting `verb` to `user` on
    /// one artifact instance.
    pub fn share(user: Ed25519PublicKey, instance: ArtifactId, verb: Verb) -> Self {
        Self::new(
            Subject::User(user),
            Scope::ResourceInstance(instance),
            Effect::Grant,
            verb,
        )
    }

    /// Convenience: cap a role at `verb` within an app.
    pub fn restrict_role_in_app(role: Role, app: impl Into<String>, verb: Verb) -> Self {
        Self::new(
            Subject::Role(role),
            Scope::App(app.into()),
            Effect::Restrict,
            verb,
        )
    }

    /// Convenience: grant `verb` to `subject` on `scope`.
    pub fn grant(subject: Subject, scope: Scope, verb: Verb) -> Self {
        Self::new(subject, scope, Effect::Grant, verb)
    }

    /// Convenience: room membership — the capability that *is* being in
    /// a room. Deliberately spelled as an ordinary grant, because that
    /// is exactly what it is.
    pub fn membership(subject: Subject, room: impl Into<String>, verb: Verb) -> Self {
        Self::grant(subject, Scope::Room(room.into()), verb)
    }

    /// Attach a stable id.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Attach a wall-clock expiry (seconds since the Unix epoch).
    pub fn with_expiry(mut self, unix_seconds: u64) -> Self {
        self.expiry = Expiry::AtUnix(unix_seconds);
        self
    }

    /// Attach any [`Expiry`].
    pub fn expiring(mut self, expiry: Expiry) -> Self {
        self.expiry = expiry;
        self
    }

    /// Record who granted this.
    pub fn granted_by(mut self, who: Principal) -> Self {
        self.granted_by = Some(who);
        self
    }

    /// Is this capability live at `now` (seconds since the Unix epoch)?
    ///
    /// `now: None` means "do not evaluate expiry" — used by the wire
    /// validator, which has no trustworthy clock shared with the author
    /// and must not make replay depend on local time.
    pub fn is_live_at(&self, now: Option<u64>) -> bool {
        self.expiry.is_live_at(now)
    }

    /// Should this capability be written to disk?
    pub fn persists_across_restart(&self) -> bool {
        self.expiry.persists_across_restart()
    }

    /// Does this capability name a person as its subject?
    ///
    /// The hard UI constraint keys off this: capabilities whose subject
    /// is a person and whose scope is a single resource belong on *that
    /// resource's* share list, never in the room roster.
    pub fn subject_is_person(&self) -> bool {
        match &self.subject {
            Subject::User(_) => true,
            Subject::Principal(p) => p.is_person(),
            Subject::Role(_) => false,
        }
    }

    /// Is this capability room membership — i.e. does it belong in the
    /// roster rather than on a resource's share list?
    ///
    /// Exactly the capabilities scoped to a [`Scope::Room`] (or the
    /// whole workspace) qualify. A person holding `FsPath` or
    /// `ResourceInstance` capabilities and nothing else is **not** a
    /// member and must never be rendered as one.
    pub fn is_room_membership(&self) -> bool {
        matches!(self.scope, Scope::Room(_) | Scope::Workspace)
    }
}

/// The named cap-bundle for a role.
///
/// Coherent with [`crate::role::role_permits`]: the returned verb is
/// the ceiling the role holds workspace-wide, and every op class the
/// table permits for the role is implied by it (proven in
/// `tests/caps.rs::caps_agree_with_role_table`). Returns `None` for
/// [`Role::Denied`] — an explicit block holds no caps.
pub fn role_caps(role: Role) -> Option<Verb> {
    match role {
        Role::Owner => Some(Verb::Admin),
        Role::Editor => Some(Verb::Write),
        Role::Commenter => Some(Verb::Comment),
        Role::Viewer => Some(Verb::Read),
        Role::Denied => None,
    }
}

/// The verb an op class requires.
///
/// The mutation classes (`Splice`/`Move`/`EditMeta`/`Tombstone`/
/// `Restore`) require `Write`; `Comment` requires `Comment`; the
/// governance classes (`Govern`/`Compaction`/`Redact`) require `Admin`.
pub fn verb_for_opclass(op: OpClass) -> Verb {
    match op {
        OpClass::Comment => Verb::Comment,
        OpClass::Splice
        | OpClass::Move
        | OpClass::EditMeta
        | OpClass::Tombstone
        | OpClass::Restore => Verb::Write,
        OpClass::Govern | OpClass::Compaction | OpClass::Redact => Verb::Admin,
    }
}

/// The outcome of a capability decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapDecision {
    /// Permitted.
    Allow,
    /// Denied because the actor's role (its base cap-bundle) does not
    /// confer the verb and no grant extends it. Maps to the existing
    /// `RoleForbidsOpClass` validation error so the coarse role table's
    /// behaviour is preserved byte-for-byte.
    DeniedByRole,
    /// Denied because a `Restrict` override capped authority within the
    /// matched scope below the required verb — even though the role (or
    /// a grant) would otherwise have allowed it.
    DeniedByRestriction,
}

/// Decide whether an actor may perform `required` on a resource.
///
/// Inputs:
/// - `base_role_allows`: the coarse role-table answer
///   ([`crate::role::role_permits`]) for this `(role, kind, op)`. Passed
///   in rather than recomputed so this crate keeps a single source of
///   truth for the base table.
/// - `required`: the verb the op needs ([`verb_for_opclass`]).
/// - `role` / `author`: the actor, for subject matching.
/// - `ctx`: the op's resource context, for scope matching.
/// - `overrides`: the channel's `(subject × scope × verb)` overrides.
///
/// Semantics (fail-closed, deny-biased):
/// - A `Denied` role is an explicit block: always [`CapDecision::DeniedByRole`].
/// - `effective = (base_role_allows OR granted) AND NOT restricted`.
///   Grants extend the base; a matching restriction removes authority
///   regardless of grants.
/// - If not allowed: [`CapDecision::DeniedByRole`] when neither the base
///   nor a grant offered the verb (a pure role denial), otherwise
///   [`CapDecision::DeniedByRestriction`].
pub fn decide(
    base_role_allows: bool,
    required: Verb,
    role: Role,
    author: Ed25519PublicKey,
    ctx: &ResourceScope,
    overrides: &[CapOverride],
) -> CapDecision {
    decide_actor(
        base_role_allows,
        required,
        &Actor::signed(role, author),
        &ResourceRef::Artifact {
            app: ctx.app.clone(),
            kind: ctx.kind,
            instance: ctx.instance,
        },
        overrides,
        // The wire validator has no clock it shares with the op's
        // author. Evaluating expiry here would make two peers disagree
        // about the validity of the same signed op, so expiry is a
        // *local* enforcement concern only.
        None,
    )
}

/// Decide whether `actor` may perform `required` on `resource` — the
/// general form, and the one every non-wire enforcement point calls.
///
/// Same fail-closed, deny-biased semantics as [`decide`], generalised
/// on three axes:
/// - the actor may be an app, agent or bearer token, not only a keyed
///   person (see [`Actor`]);
/// - the resource may be a path, device, host or action, not only a
///   CRDT artifact (see [`ResourceRef`]);
/// - capabilities expire. `now: Some(unix_seconds)` drops expired ones
///   before deciding; `None` skips expiry evaluation entirely.
///
/// An actor with `role: None` — every app, agent and token — has no
/// base authority whatsoever, so it can act *only* through an explicit
/// capability. That is the property that makes the portal prompt the
/// only user-facing permission dialog in the system.
pub fn decide_actor(
    base_role_allows: bool,
    required: Verb,
    actor: &Actor,
    resource: &ResourceRef,
    caps: &[CapOverride],
    now: Option<u64>,
) -> CapDecision {
    // An explicit block overrides any inherited or shared grant.
    if actor.role == Some(Role::Denied) {
        return CapDecision::DeniedByRole;
    }

    let mut granted = false;
    let mut restricted = false;
    for ov in caps {
        if !ov.is_live_at(now)
            || !ov.subject.matches_actor(actor)
            || !ov.scope.matches_resource(resource)
        {
            continue;
        }
        match ov.effect {
            Effect::Grant if ov.verb.implies(required) => granted = true,
            Effect::Restrict if !ov.verb.implies(required) => restricted = true,
            _ => {}
        }
    }

    let allowed = (base_role_allows || granted) && !restricted;
    if allowed {
        CapDecision::Allow
    } else if !base_role_allows && !granted {
        CapDecision::DeniedByRole
    } else {
        CapDecision::DeniedByRestriction
    }
}

/// Does a role's cap-bundle confer *any* base authority over this kind
/// of resource?
///
/// Roles describe standing in a room, so they carry base authority over
/// the room and its contents: the workspace itself, its apps, their
/// artifacts and exported actions.
///
/// They carry **none** over the filesystem, devices, or the network.
/// Those live outside the room — `/etc/shadow` is not "content of the
/// workspace an Editor may edit" — so reaching them always requires an
/// explicit capability, even for an Owner. The workspace's own file
/// tree is reached the same way: by an explicit `FsPath` capability
/// scoped to the workspace root, granted when the room is created.
///
/// This is the single most load-bearing line in the model. Without it,
/// `role_caps(Editor) == Write` would silently authorise writing every
/// path on the machine.
pub fn role_base_applies(resource: &ResourceRef) -> bool {
    match resource {
        ResourceRef::Workspace
        | ResourceRef::App(_)
        | ResourceRef::Artifact { .. }
        | ResourceRef::Action { .. }
        | ResourceRef::Room(_) => true,
        ResourceRef::FsPath(_) | ResourceRef::Device(_) | ResourceRef::NetHost(_) => false,
    }
}

/// [`decide_actor`] with the base authority derived from the actor's
/// own role rather than passed in.
///
/// The role's cap-bundle ([`role_caps`]) is the base, but only over
/// resources [`role_base_applies`] admits; an actor with no role never
/// has a base. This is the entry point for every local check — in-app
/// actions, file access, device access — where there is no separate
/// op-class table to consult.
pub fn decide_capability(
    actor: &Actor,
    resource: &ResourceRef,
    required: Verb,
    caps: &[CapOverride],
    now: Option<u64>,
) -> CapDecision {
    let base = role_base_applies(resource)
        && actor
            .role
            .and_then(role_caps)
            .is_some_and(|ceiling| ceiling.implies(required));
    decide_actor(base, required, actor, resource, caps, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implies_ladder() {
        assert!(Verb::Admin.implies(Verb::Invite));
        assert!(Verb::Admin.implies(Verb::Write));
        assert!(Verb::Write.implies(Verb::Comment));
        assert!(Verb::Write.implies(Verb::Read));
        assert!(!Verb::Write.implies(Verb::Admin));
        assert!(!Verb::Comment.implies(Verb::Write));
        assert!(Verb::Comment.implies(Verb::Read));
        assert!(Verb::Invite.implies(Verb::Read));
        assert!(!Verb::Invite.implies(Verb::Write));
        assert!(Verb::Read.implies(Verb::Read));
        assert!(!Verb::Read.implies(Verb::Comment));
    }

    #[test]
    fn role_caps_match_verb_ceiling() {
        assert_eq!(role_caps(Role::Owner), Some(Verb::Admin));
        assert_eq!(role_caps(Role::Editor), Some(Verb::Write));
        assert_eq!(role_caps(Role::Commenter), Some(Verb::Comment));
        assert_eq!(role_caps(Role::Viewer), Some(Verb::Read));
        assert_eq!(role_caps(Role::Denied), None);
    }
}
