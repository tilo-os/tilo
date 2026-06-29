//! Roles, op classes, and the spec's static permission table.
//!
//! `Role` and `OpClass` are the two enums that drive every "is this
//! mutation allowed?" decision in the system. `role_permits` is the
//! lookup function — the single source of truth, consulted by both
//! the local shell (to paint affordances and grey out forbidden
//! actions) and every receiving peer's validator (to reject invalid
//! ops on the wire).
//!
//! The table is intentionally hard-coded at compile time. Adding a
//! new role or op class is a schema-breaking change that needs a
//! version bump on every peer, so we don't pretend it is data.

use serde::{Deserialize, Serialize};

/// Roles a member can hold in a channel.
///
/// Roles are ordered logically (Owner most privileged, Denied least)
/// but `Ord` is **not** derived: ordering by enum discriminant would
/// be misleading because `Denied` is not "less than Viewer" — it is
/// an *explicit block* that overrides any inherited grant from the
/// channel's `default_role`. Callers comparing privilege must do so
/// against the permission table, not by ordering roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Role {
    /// Channel governance authority. Can issue + revoke
    /// attestations, transfer ownership, edit channel settings, and
    /// — by extension — perform every op class on every artifact.
    Owner,
    /// Mutate any artifact, but cannot govern. Editors are the
    /// default role for trust-rich teams.
    Editor,
    /// Append-only. Can comment + react but never splice, move,
    /// rename, or delete.
    Commenter,
    /// Read-only. No signed ops accepted from this role. Used for
    /// auditors, observers, and lurkers.
    Viewer,
    /// Explicit block. Overrides any inherited grant. Used to
    /// kick-but-not-delete: the member remains in the channel
    /// directory (so their historical attribution survives) but
    /// cannot apply any op of any class.
    Denied,
}

/// The semantic class of a single CRDT op, declared in the op
/// envelope itself.
///
/// The classification is **author-declared**, not validator-derived,
/// so a malicious client could lie. The validator catches lies by
/// re-checking the role table; the broker never relies on the
/// declared class for routing, only for fan-out filtering at coarse
/// granularity (e.g. only forwarding Comment-class ops to
/// Commenter-tier members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum OpClass {
    /// Text edit. Insert/delete bytes into a CRDT text field.
    Splice,
    /// Reorder operation. Kanban card moves, file reordering,
    /// strip-swap in the compositor model.
    Move,
    /// Metadata mutation. Rename, retag, change description.
    EditMeta,
    /// Append a comment or reaction. The Commenter role's *only*
    /// permitted class.
    Comment,
    /// Soft delete. Marks the artifact as deleted but preserves
    /// history; the artifact's data and op log remain.
    Tombstone,
    /// Undo a tombstone. Available to Editor+ on every artifact
    /// kind.
    Restore,
    /// Governance op. Issue/revoke attestations, transfer ownership,
    /// rename channel, update channel settings. Only Owner role
    /// permits this class.
    Govern,
}

/// The categorical kind of an artifact.
///
/// Each kind has its own row in the permission table because
/// "Comment on a Kanban" means something different from
/// "Comment on a Note" — the latter is a side-anchor on text, the
/// former is a card-attached thread. The toolkit handles both
/// uniformly; the permission rule is the same shape either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ArtifactKind {
    /// Free-form rich-text document. The qOS Notes app's primitive.
    Note,
    /// Binary file backed by the P2P FUSE layer. Op classes that
    /// "mutate" are interpreted as filesystem operations.
    File,
    /// A compositor cluster session — a tiled set of running
    /// windows that can be saved + reopened collaboratively.
    Cluster,
    /// Kanban board. `Move` is a first-class op for card
    /// transitions; `Splice` lands on card body text.
    Kanban,
    /// Append-only chat log inside a channel.
    ChatLog,
}

/// The spec's static permission table.
///
/// Returns `true` iff a member holding `role` is permitted to
/// emit an op of `op` class against an artifact of `kind`.
/// Owner permits everything (including Govern). Editor permits
/// every class except Govern, on every artifact kind. Commenter
/// permits Comment only. Viewer and Denied permit nothing.
///
/// The table is exhaustive — every (Role, ArtifactKind, OpClass)
/// triple has a defined answer. The tests in `tests/role_table.rs`
/// cross-check every row.
///
/// Stability: this is a wire-protocol decision. Adding a row
/// (e.g. a new ArtifactKind) is allowed; flipping an existing
/// row's value is a breaking change because two peers running
/// different versions would disagree on whether an op is valid.
pub fn role_permits(role: Role, _kind: ArtifactKind, op: OpClass) -> bool {
    // The implementation could in principle vary per artifact kind
    // (e.g. "Comment on a File doesn't exist"), but the spec's
    // table treats every kind uniformly: the role determines the
    // permitted op classes, the kind doesn't subtract from that.
    // If a future row needs to be kind-specific, branch here on
    // `_kind` and document the carve-out in the spec first.
    match role {
        Role::Owner => true,
        Role::Editor => match op {
            OpClass::Splice
            | OpClass::Move
            | OpClass::EditMeta
            | OpClass::Comment
            | OpClass::Tombstone
            | OpClass::Restore => true,
            OpClass::Govern => false,
        },
        Role::Commenter => matches!(op, OpClass::Comment),
        Role::Viewer => false,
        Role::Denied => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_can_do_everything() {
        for kind in [
            ArtifactKind::Note,
            ArtifactKind::File,
            ArtifactKind::Cluster,
            ArtifactKind::Kanban,
            ArtifactKind::ChatLog,
        ] {
            for op in [
                OpClass::Splice,
                OpClass::Move,
                OpClass::EditMeta,
                OpClass::Comment,
                OpClass::Tombstone,
                OpClass::Restore,
                OpClass::Govern,
            ] {
                assert!(
                    role_permits(Role::Owner, kind, op),
                    "Owner should permit {:?} on {:?}",
                    op,
                    kind
                );
            }
        }
    }

    #[test]
    fn editor_cannot_govern() {
        assert!(!role_permits(
            Role::Editor,
            ArtifactKind::Note,
            OpClass::Govern
        ));
    }

    #[test]
    fn commenter_only_comment() {
        for op in [
            OpClass::Splice,
            OpClass::Move,
            OpClass::EditMeta,
            OpClass::Tombstone,
            OpClass::Restore,
            OpClass::Govern,
        ] {
            assert!(!role_permits(Role::Commenter, ArtifactKind::Note, op));
        }
        assert!(role_permits(
            Role::Commenter,
            ArtifactKind::Note,
            OpClass::Comment
        ));
    }

    #[test]
    fn viewer_nothing() {
        for op in [
            OpClass::Splice,
            OpClass::Move,
            OpClass::EditMeta,
            OpClass::Comment,
            OpClass::Tombstone,
            OpClass::Restore,
            OpClass::Govern,
        ] {
            assert!(!role_permits(Role::Viewer, ArtifactKind::Kanban, op));
        }
    }

    #[test]
    fn denied_nothing() {
        for op in [
            OpClass::Splice,
            OpClass::Move,
            OpClass::EditMeta,
            OpClass::Comment,
            OpClass::Tombstone,
            OpClass::Restore,
            OpClass::Govern,
        ] {
            assert!(!role_permits(Role::Denied, ArtifactKind::File, op));
        }
    }
}
