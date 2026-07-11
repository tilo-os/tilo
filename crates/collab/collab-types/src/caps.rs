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

/// Who a permission is about.
///
/// A permission's subject is either a whole role (the cap-bundle every
/// holder of that role inherits) or one specific identity (an
/// individual override / per-resource share).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Subject {
    /// Everyone holding this role in the channel.
    Role(Role),
    /// One specific identity, by public key.
    User(Ed25519PublicKey),
}

impl Subject {
    /// Does this subject cover an actor with role `role` and key
    /// `author`?
    pub fn matches(&self, role: Role, author: Ed25519PublicKey) -> bool {
        match self {
            Subject::Role(r) => *r == role,
            Subject::User(pk) => *pk == author,
        }
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
    FsPath(String),
}

impl Scope {
    /// Does this scope apply to an op with the given resource context?
    pub fn matches_op(&self, ctx: &ResourceScope) -> bool {
        match self {
            Scope::Workspace => true,
            Scope::App(a) => ctx.app.as_deref() == Some(a.as_str()),
            Scope::ResourceType(k) => ctx.kind == *k,
            Scope::ResourceInstance(i) => ctx.instance == *i,
            // Filesystem-path scopes do not describe CRDT ops.
            Scope::FsPath(_) => false,
        }
    }
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

/// One permission override: `(subject × scope × verb)` plus whether it
/// grants or restricts.
///
/// A `Grant { verb: Comment }` says "this subject may (at least)
/// comment within this scope". A `Restrict { verb: Comment }` says
/// "this subject may do **no more than** comment within this scope" —
/// any required verb not implied by `Comment` is removed.
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
}

impl CapOverride {
    /// Convenience: a per-resource share granting `verb` to `user` on
    /// one artifact instance.
    pub fn share(user: Ed25519PublicKey, instance: ArtifactId, verb: Verb) -> Self {
        Self {
            subject: Subject::User(user),
            scope: Scope::ResourceInstance(instance),
            effect: Effect::Grant,
            verb,
        }
    }

    /// Convenience: cap a role at `verb` within an app.
    pub fn restrict_role_in_app(role: Role, app: impl Into<String>, verb: Verb) -> Self {
        Self {
            subject: Subject::Role(role),
            scope: Scope::App(app.into()),
            effect: Effect::Restrict,
            verb,
        }
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
    // An explicit block overrides any inherited or shared grant.
    if role == Role::Denied {
        return CapDecision::DeniedByRole;
    }

    let mut granted = false;
    let mut restricted = false;
    for ov in overrides {
        if !ov.subject.matches(role, author) || !ov.scope.matches_op(ctx) {
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
