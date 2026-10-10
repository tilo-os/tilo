//! The capability model — `(subject × scope × verb)`.
//!
//! [`crate::role::role_permits`] answers the coarse "may this role emit this
//! op class?" and stays the authoritative wire decision; this lets a
//! permission be expressed at finer granularity on top of it. [`role_caps`]
//! must stay coherent with `role_permits` — checked by
//! `caps_agree_with_role_table` in `tests/caps.rs`.
//!
//! [`decide`] is fail-closed and deny-biased: no base authority + no
//! matching grant denies; a `Denied` role can never be rescued by a grant;
//! a matching `Restrict` removes authority even if a `Grant` would allow it.

use serde::{Deserialize, Serialize};
pub mod action_authority;
pub mod net;

pub use net::{Allowed, EgressPolicy, HostPattern, NetHostError, NetHostPattern, host_covers};

use crate::channel::ArtifactId;
use crate::crypto::Ed25519PublicKey;
use crate::role::{ArtifactKind, OpClass, Role};

/// A coarse capability verb.
///
/// The four coarse caps from the coherence doc (`read/write/invite/admin`)
/// plus `Comment` (between `Read` and `Write` — the `Commenter` role may
/// comment but not edit). Not a single linear order — `Invite` is
/// governance-ish, orthogonal to the `Read < Comment < Write` ladder — so
/// containment is expressed by [`Verb::implies`] rather than `Ord`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Verb {
    /// See the resource. The floor: every other verb implies it.
    Read,
    /// Append a comment or reaction. Implies `Read`.
    Comment,
    /// Mutate content (splice/move/rename/tombstone/restore). Implies `Comment` + `Read`.
    Write,
    /// Admit a member / mint an invite token. Implies `Read` but is otherwise
    /// independent of the content ladder.
    Invite,
    /// Govern: issue/revoke attestations, compaction, redaction, settings.
    /// The ceiling — implies every other verb.
    Admin,
}

impl Verb {
    /// Does holding `self` confer authority for `other`?
    ///
    /// Containment, not ordering: `Admin` confers everything, `Write` confers
    /// `Comment`/`Read`, `Invite` confers only `Read` besides itself. Lets a
    /// `Grant`/`Restrict` phrased in a coarse verb answer a finer required verb.
    pub fn implies(self, other: Verb) -> bool {
        use Verb::{Admin, Comment, Invite, Read, Write};
        match self {
            Admin => true,
            Write => matches!(other, Write | Comment | Read),
            Invite => matches!(other, Invite | Read),
            Comment => matches!(other, Comment | Read),
            Read => matches!(other, Read),
        }
    }
}

/// A named principal — the string-keyed half of the subject space, for
/// enforcement points with no signature to check (app-id, agent name,
/// bearer token, room).
///
/// The kind tag is load-bearing: a share list renders [`Principal::Person`]
/// rows, a workspace roster renders room membership, and they must never
/// become the same list — see [`Principal::is_person`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Principal {
    /// A human, by stable id. When the person has a signing key the id is its
    /// lowercase hex, so a `Person` principal and a matching [`Subject::User`]
    /// are interchangeable.
    Person(String),
    /// An application, by its stable app id. Subject of the *only* case that
    /// prompts the user: an app reaching outside what it owns.
    App(String),
    /// An automated agent — same shape as an app, tagged distinctly so it can
    /// be listed and revoked as one.
    Agent(String),
    /// A bearer token: whoever presents this token id holds the capability.
    Token(String),
    /// A room (workspace). A capability *subject* of a room is held by
    /// everyone in it; a capability *resource* of a room is membership itself.
    Room(String),
}

impl Principal {
    /// Name a person by their Ed25519 public key (canonical lowercase hex),
    /// keeping `Subject::User` and `Principal::Person` interchangeable.
    pub fn person_from_key(key: Ed25519PublicKey) -> Self {
        Principal::Person(hex::encode(key.0))
    }

    /// Is this principal a human?
    ///
    /// Keys the share-list/roster split: a capability whose subject is a
    /// person belongs on the resource's share list; only room membership
    /// belongs in the roster.
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

/// Who a permission is about: a whole role (the cap-bundle every holder
/// inherits), one specific signing identity, or a named [`Principal`].
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
    /// Does this subject cover an actor with role `role` and key `author`?
    /// Signature-only form for the wire validator; [`Subject::matches_actor`]
    /// is the general form.
    pub fn matches(&self, role: Role, author: Ed25519PublicKey) -> bool {
        self.matches_actor(&Actor::signed(role, author))
    }

    /// Does this subject cover `actor`?
    ///
    /// A `Role` subject matches only an actor that *holds* a role — an app or
    /// bearer token carries none, so a role-wide grant never covers it.
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
}

/// The thing asking to act.
///
/// Presents *at most one* role (people in a room have one; apps, agents,
/// tokens have none) plus every [`Principal`] it can prove it is. `role:
/// None` is fail-closed: no role means no base authority.
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

    /// An application. Holds no role — acts only through explicit
    /// capabilities, which is what makes the portal prompt the only
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
}

/// What a permission ranges over.
///
/// Ordered coarsest (`Workspace`) to finest (`ResourceInstance`/`FsPath`).
/// `FsPath` is meaningful only to the filesystem enforcement point and
/// never matches a CRDT op's `ResourceScope` — the two enforcement points
/// share a vocabulary but only the scopes that apply to them bite.
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
    /// Matching is recursive and component-aligned: `/w/docs` covers
    /// `/w/docs/a/b.txt` but not `/w/docsX`.
    FsPath(String),
    /// A hardware/system device class the portal brokers: `"camera"`,
    /// `"microphone"`, `"screencast"`. Filesystem- and op-inert.
    Device(String),
    /// An outbound network host, stored as written and read through
    /// [`NetHostPattern`]: `"*"` matches every host; a leading dot (or `*.`)
    /// matches the domain and subdomains; anything else is an exact hostname.
    /// An IP literal is not a host and matches nothing.
    NetHost(String),
    /// One action on one resource an app exports, `(app, resource, action)`
    /// — the finest scope: exactly one verb-bearing operation on one
    /// resource of one app's surface. The same action id on another
    /// resource (a page body's `apply` and the notebook's `apply`) is a
    /// different action and is not covered.
    Action {
        app: String,
        resource: String,
        action: String,
    },
    /// One action on every resource of one kind an app exports,
    /// `(app, kind, action)`: what a shortcut step that picks its resource
    /// when it runs (the first of a kind, a bound value) needs. Only a
    /// shortcut's grant is evaluated against it, where the action's
    /// descriptor names the resource's kind; against any other resource it
    /// is inert.
    ActionOnKind {
        app: String,
        kind: String,
        action: String,
    },
    /// A room as a *resource*. A capability scoped to a room, subject a
    /// person, **is** that person's room membership — the same machinery as
    /// any other share, so there is no second sharing system.
    Room(String),
    /// One stable collaborative-file record. Unlike [`Scope::FsPath`], this
    /// authority follows renames and moves and can safely key replication.
    FsRecord(String),
}

impl Scope {
    /// Does this scope cover `resource`?
    ///
    /// Scopes nest: `Workspace` covers everything, `App(a)` covers every
    /// artifact/action of `a`, `FsPath(p)` covers every path under `p`. A
    /// scope from a different enforcement point than the resource simply
    /// doesn't match — inert, never permissive.
    #[allow(
        clippy::unnested_or_patterns,
        reason = "the `App`/`Artifact`/`Action` alternatives already share the `Scope::App(a)` \
                  first component; they cannot nest further since each binds `b` out of a \
                  different resource shape (a bare tuple field vs. an `Option`-wrapped struct \
                  field vs. a plain struct field)"
    )]
    #[allow(
        clippy::match_same_arms,
        reason = "several arms reduce to a bare `x == y` (app id, device id, resource kind, \
                  resource instance), which looks like duplication to a body-shape check but \
                  each compares a different domain under a different Scope/ResourceRef pairing; \
                  merging them would erase which pairing licenses the match"
    )]
    pub fn matches_resource(&self, resource: &ResourceRef) -> bool {
        match (self, resource) {
            (Scope::Workspace, _) => true,

            (
                Scope::App(a),
                ResourceRef::App(b)
                | ResourceRef::Artifact { app: Some(b), .. }
                | ResourceRef::Action { app: b, .. },
            ) => a == b,

            (Scope::ResourceType(k), ResourceRef::Artifact { kind, .. }) => k == kind,

            (Scope::ResourceInstance(i), ResourceRef::Artifact { instance, .. }) => i == instance,

            (Scope::FsPath(prefix), ResourceRef::FsPath(path)) => path_covers(prefix, path),

            (Scope::Device(d), ResourceRef::Device(e)) => d == e,

            (Scope::NetHost(h), ResourceRef::NetHost(target)) => host_covers(h, target),

            (
                Scope::Action {
                    app,
                    resource,
                    action,
                },
                ResourceRef::Action {
                    app: b,
                    resource: r,
                    action: c,
                },
            ) => app == b && resource == r && action == c,

            // Room membership is converted into an actor role by `Workspace`
            // before an action is checked. Direct scope matching remains narrow:
            // a room grant cannot masquerade as a filesystem/app/action grant.
            (Scope::Room(r), ResourceRef::Room(s)) => r == s,
            (Scope::FsRecord(r), ResourceRef::FsRecord(s)) => r == s,

            // Every scope variant is paired with its own condition above; a
            // resource shape that doesn't match the scope's own kind is never
            // covered by it.
            (
                Scope::App(_)
                | Scope::ResourceType(_)
                | Scope::ResourceInstance(_)
                | Scope::FsPath(_)
                | Scope::Device(_)
                | Scope::NetHost(_)
                | Scope::Action { .. }
                | Scope::ActionOnKind { .. }
                | Scope::Room(_)
                | Scope::FsRecord(_),
                _,
            ) => false,
        }
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

/// A resource a subject might act on. Resources nest — see
/// [`Scope::matches_resource`]. Showing one needs no capability; only
/// acting on one is checked.
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
    /// One exported action on one resource of one app.
    Action {
        app: String,
        resource: String,
        action: String,
    },
    /// A room, as the target of a membership capability.
    Room(String),
    /// A stable collaborative-file record id.
    FsRecord(String),
}

/// The resource context an op carries, used to match [`Scope`]s. Built by
/// the validator from the op's artifact + the channel's artifact maps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceScope {
    /// The app that owns the artifact, if the channel records one.
    pub app: Option<String>,
    /// The artifact's kind.
    pub kind: ArtifactKind,
    /// The artifact's id.
    pub instance: ArtifactId,
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
/// grant-or-restrict.
///
/// The whole primitive — room membership, a file share, a portal grant
/// are all just a `CapOverride` with a different scope/subject; there is
/// no second sharing system. `Restrict { verb }` caps authority at "no
/// more than `verb`": any required verb it doesn't imply is removed.
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
    /// Stable id, so a specific capability can be revoked by name even after
    /// replicating to peers that never saw it granted. Empty means unnamed —
    /// revocable only by matching content.
    pub id: String,
    /// When this capability dies. An expired capability is invisible to
    /// [`decide_actor`] — not "denied", simply not there.
    pub expiry: Expiry,
    /// Who granted this, for the share-list UI and audit. Advisory: grant
    /// authority is enforced where the grant is *made*, not here.
    pub granted_by: Option<Principal>,
}

impl CapOverride {
    /// The general constructor. Prefer the named helpers below — they read
    /// better at the call site.
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

    /// Convenience: room membership — the capability that *is* being in a
    /// room. Spelled as an ordinary grant, because that's exactly what it is.
    pub fn membership(subject: Subject, room: impl Into<String>, verb: Verb) -> Self {
        Self::grant(subject, Scope::Room(room.into()), verb)
    }

    /// Attach a stable id.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Attach a wall-clock expiry (seconds since the Unix epoch).
    #[must_use]
    pub fn with_expiry(mut self, unix_seconds: u64) -> Self {
        self.expiry = Expiry::AtUnix(unix_seconds);
        self
    }

    /// Attach any [`Expiry`].
    #[must_use]
    pub fn expiring(mut self, expiry: Expiry) -> Self {
        self.expiry = expiry;
        self
    }

    /// Record who granted this.
    #[must_use]
    pub fn granted_by(mut self, who: Principal) -> Self {
        self.granted_by = Some(who);
        self
    }

    /// Is this capability live at `now`? `None` means "do not evaluate
    /// expiry" — the wire validator has no trustworthy shared clock and must
    /// not make replay depend on local time.
    pub fn is_live_at(&self, now: Option<u64>) -> bool {
        self.expiry.is_live_at(now)
    }

    /// Should this capability be written to disk?
    pub fn persists_across_restart(&self) -> bool {
        self.expiry.persists_across_restart()
    }

    /// Does this capability name a person as its subject? Keys the hard UI
    /// constraint: a person-subject, single-resource capability belongs on
    /// that resource's share list, never the room roster.
    pub fn subject_is_person(&self) -> bool {
        match &self.subject {
            Subject::User(_) => true,
            Subject::Principal(p) => p.is_person(),
            Subject::Role(_) => false,
        }
    }

    /// Is this capability room membership — i.e. roster, not a resource's
    /// share list?
    ///
    /// Only capabilities scoped to [`Scope::Room`] (or the whole workspace)
    /// qualify. A person holding only `FsPath`/`ResourceInstance` capabilities
    /// is **not** a member and must never render as one.
    pub fn is_room_membership(&self) -> bool {
        matches!(self.scope, Scope::Room(_) | Scope::Workspace)
    }
}

/// The named cap-bundle for a role.
///
/// Coherent with [`crate::role::role_permits`]: the returned verb is the
/// role's workspace-wide ceiling (proven in
/// `tests/caps.rs::caps_agree_with_role_table`). `None` for
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

/// The verb an op class requires: mutation classes
/// (`Splice`/`Move`/`EditMeta`/`Tombstone`/`Restore`) need `Write`;
/// `Comment` needs `Comment`; governance classes
/// (`Govern`/`Compaction`/`Redact`) need `Admin`.
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
    /// Denied because the actor's role doesn't confer the verb and no grant
    /// extends it. Maps to `RoleForbidsOpClass` so the coarse role table's
    /// behaviour is preserved byte-for-byte.
    DeniedByRole,
    /// Denied because a `Restrict` override capped authority below the
    /// required verb, even though role or grant would otherwise allow it.
    DeniedByRestriction,
}

/// Decide whether an actor may perform `required` on a resource.
///
/// `base_role_allows` is the coarse role-table answer
/// ([`crate::role::role_permits`]), passed in so this crate keeps one
/// source of truth for the base table.
///
/// Fail-closed, deny-biased: a `Denied` role is always
/// [`CapDecision::DeniedByRole`]. Otherwise `effective = (base_role_allows
/// OR granted) AND NOT restricted` — a matching restriction removes
/// authority regardless of grants.
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
        // The wire validator has no clock it shares with the op's author.
        // Evaluating expiry here would make two peers disagree about the same
        // signed op, so expiry is a *local* enforcement concern only.
        None,
    )
}

/// Decide whether `actor` may perform `required` on `resource` — the
/// general form every non-wire enforcement point calls.
///
/// Same fail-closed, deny-biased semantics as [`decide`], generalised to
/// [`Actor`]/[`ResourceRef`]. An actor with `role: None` — every app,
/// agent and token — has no base authority, so it can act *only* through
/// an explicit capability: the property that makes the portal prompt the
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
        // App/agent workspace grants authorize room authorship, not external
        // storage, devices or network. In particular they must never upgrade
        // an explicit read-only file grant. Keep workspace restrictions broad
        // so read-only participation still clamps all writes; unrestricted
        // local-user grants retain their existing semantics.
        if ov.effect == Effect::Grant
            && ov.scope == Scope::Workspace
            && matches!(
                ov.subject,
                Subject::Principal(Principal::App(_) | Principal::Agent(_))
            )
            && !role_base_applies(resource)
        {
            continue;
        }
        match ov.effect {
            Effect::Grant if ov.verb.implies(required) => granted = true,
            Effect::Restrict if !ov.verb.implies(required) => restricted = true,
            _ => {}
        }
    }

    restricted |= crate::foreign_device::exceeds_ceiling(actor, required);
    let allowed = (base_role_allows || granted) && !restricted;
    if allowed {
        CapDecision::Allow
    } else if !base_role_allows && !granted {
        CapDecision::DeniedByRole
    } else {
        CapDecision::DeniedByRestriction
    }
}

/// Does a role's cap-bundle confer *any* base authority over this kind of
/// resource?
///
/// Roles carry base authority over the room and its contents (workspace,
/// apps, artifacts, actions), but **none** over the filesystem, devices,
/// or network — those live outside the room, so reaching them always
/// requires an explicit capability, even for an Owner. The workspace's
/// own file tree is reached the same way, via an explicit `FsPath`
/// capability scoped to the root and granted at room creation.
///
/// This is the single most load-bearing line in the model: without it,
/// `role_caps(Editor) == Write` would silently authorise writing every
/// path on the machine.
pub fn role_base_applies(resource: &ResourceRef) -> bool {
    match resource {
        ResourceRef::Workspace
        | ResourceRef::App(_)
        | ResourceRef::Artifact { .. }
        | ResourceRef::Action { .. }
        | ResourceRef::Room(_) => true,
        ResourceRef::FsPath(_)
        | ResourceRef::FsRecord(_)
        | ResourceRef::Device(_)
        | ResourceRef::NetHost(_) => false,
    }
}

/// [`decide_actor`] with base authority derived from the actor's own role
/// rather than passed in. [`role_caps`] is the base, but only over
/// resources [`role_base_applies`] admits; an actor with no role never
/// has one. Entry point for every local check with no separate op-class
/// table to consult.
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
    fn workspace_authorship_cannot_upgrade_external_file_device_or_network_grants() {
        for (actor, subject) in [
            (Actor::app("so.tilo.books"), Subject::app("so.tilo.books")),
            (Actor::agent("reader"), Subject::agent("reader")),
        ] {
            let caps = vec![
                CapOverride::grant(subject.clone(), Scope::Workspace, Verb::Write),
                CapOverride::grant(
                    subject,
                    Scope::FsPath("/approved/book.txt".into()),
                    Verb::Read,
                ),
            ];
            assert_eq!(
                decide_capability(&actor, &ResourceRef::Workspace, Verb::Write, &caps, None),
                CapDecision::Allow
            );
            assert_eq!(
                decide_capability(
                    &actor,
                    &ResourceRef::FsPath("/approved/book.txt".into()),
                    Verb::Read,
                    &caps,
                    None,
                ),
                CapDecision::Allow
            );
            for resource in [
                ResourceRef::FsPath("/approved/book.txt".into()),
                ResourceRef::FsPath("/approved/sibling.txt".into()),
                ResourceRef::Device("camera".into()),
                ResourceRef::NetHost("example.com".into()),
                ResourceRef::FsRecord("file-1".into()),
            ] {
                assert_ne!(
                    decide_capability(&actor, &resource, Verb::Write, &caps, None),
                    CapDecision::Allow,
                    "workspace authorship must not authorize external resource {resource:?}"
                );
            }
        }
    }

    /// Verbs imply the ones below them, and each role's cap ceiling matches its verb ceiling.
    #[test]
    fn capability_verbs_form_a_ladder_that_bounds_each_role() {
        // Phase: implies_ladder.
        {
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

        // Phase: role_caps_match_verb_ceiling.
        {
            assert_eq!(role_caps(Role::Owner), Some(Verb::Admin));
            assert_eq!(role_caps(Role::Editor), Some(Verb::Write));
            assert_eq!(role_caps(Role::Commenter), Some(Verb::Comment));
            assert_eq!(role_caps(Role::Viewer), Some(Verb::Read));
            assert_eq!(role_caps(Role::Denied), None);
        }
    }
}
