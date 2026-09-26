//! Roles, op classes, and the spec's static permission table.
//!
//! `Role` and `OpClass` drive every "is this mutation allowed?" decision.
//! `role_permits` is the single source of truth, consulted by both the
//! local shell (to paint/grey out affordances) and every receiving
//! peer's validator (to reject invalid ops on the wire).
//!
//! The table is hard-coded at compile time — adding a role or op class is
//! a schema-breaking change needing a version bump on every peer, so it
//! isn't pretended to be data.

use serde::{Deserialize, Serialize};

/// Roles a member can hold in a channel.
///
/// Ordered logically (Owner most privileged, Denied least) but `Ord` is
/// **not** derived: `Denied` is not "less than Viewer", it's an explicit
/// block overriding any inherited grant from `default_role`. Callers
/// comparing privilege must go through the permission table, not enum ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Role {
    /// Channel governance authority: issue/revoke attestations, transfer
    /// ownership, edit settings, and — by extension — every op class on every artifact.
    Owner,
    /// Mutate any artifact, but cannot govern. Default role for trust-rich teams.
    Editor,
    /// Append-only: can comment + react but never splice, move, rename, or delete.
    Commenter,
    /// Read-only. No signed ops accepted from this role — auditors,
    /// observers, lurkers.
    Viewer,
    /// Explicit block, overriding any inherited grant. Kick-but-not-delete:
    /// the member stays in the channel directory (historical attribution
    /// survives) but can apply no op of any class.
    Denied,
}

/// The semantic class of a single CRDT op, declared in the op envelope itself.
///
/// **Author-declared**, not validator-derived — a malicious client could
/// lie. The validator catches lies by re-checking the role table; the
/// broker never relies on the declared class for routing, only coarse
/// fan-out filtering (e.g. Comment-class ops only to Commenter-tier members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum OpClass {
    /// Text edit. Insert/delete bytes into a CRDT text field.
    Splice,
    /// Reorder operation: kanban card moves, file reordering, strip-swap in
    /// the compositor model.
    Move,
    /// Metadata mutation. Rename, retag, change description.
    EditMeta,
    /// Append a comment or reaction — the Commenter role's *only* permitted class.
    Comment,
    /// Soft delete: marks the artifact deleted but preserves history and the op log.
    Tombstone,
    /// Undo a tombstone. Available to Editor+ on every artifact kind.
    Restore,
    /// Governance op: issue/revoke attestations, transfer ownership, rename
    /// channel, update settings. Only Owner permits this class.
    Govern,
    /// Host-driven compaction checkpoint. The channel's host (initially the
    /// owner's primary device) emits one every N ops or M bytes; recipients
    /// use it to evict older op-log entries and snapshot state to cold
    /// history. Only `Owner` permits this class — a quorum-elected backup
    /// host is a future follow-up. Payload is a
    /// [`crate::compaction::CompactionCheckpoint`] via
    /// [`crate::canonical::canonical_bytes`].
    Compaction,
    /// Cooperative cryptographic redaction. Only the channel **owner** (see
    /// [`role_permits`]) mints a Redact op to declare "this earlier op is now
    /// compliance-required to be removed; peers, please honour it." Payload
    /// is a [`crate::redaction::RedactRequest`] via
    /// [`crate::canonical::canonical_bytes`].
    ///
    /// Recursive redaction (a `Redact` targeting another `Redact`) is
    /// rejected by the validator — see
    /// [`crate::validator::ValidationError::RecursiveRedaction`].
    Redact,
}

/// The categorical kind of an artifact.
///
/// Each kind has its own permission-table row because "Comment on a
/// Kanban" differs from "Comment on a Note" (side-anchor on text vs.
/// card-attached thread), even though the toolkit and the permission
/// rule handle both the same shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ArtifactKind {
    /// Free-form rich-text document. The Tilo Notes app's primitive.
    Note,
    /// Binary file backed by the P2P FUSE layer. "Mutate" op classes are
    /// interpreted as filesystem operations.
    File,
    /// A compositor cluster session — a tiled set of running windows saved +
    /// reopened collaboratively.
    Cluster,
    /// Kanban board. `Move` is first-class for card transitions; `Splice`
    /// lands on card body text.
    Kanban,
    /// Append-only chat log inside a channel.
    ChatLog,
}

/// The spec's static permission table: does a member holding `role` may
/// emit an op of `op` class against an artifact of `kind`?
///
/// Owner permits everything (including Govern). Editor permits every
/// class except Govern, on every kind. Commenter permits Comment only.
/// Viewer and Denied permit nothing. Exhaustive — every (Role,
/// `ArtifactKind`, `OpClass`) triple has a defined answer, cross-checked by
/// `tests/role_table.rs`.
///
/// This is a wire-protocol decision: adding a row is fine, but flipping
/// an existing row's value is a breaking change — two peers on different
/// versions would disagree on op validity.
pub fn role_permits(role: Role, _kind: ArtifactKind, op: OpClass) -> bool {
    // Could in principle vary per artifact kind, but the spec's table
    // treats every kind uniformly — role determines permitted op
    // classes, kind doesn't subtract from that. A future kind-specific
    // carve-out branches here, documented in the spec first.
    match role {
        Role::Owner => true,
        Role::Editor => match op {
            OpClass::Splice
            | OpClass::Move
            | OpClass::EditMeta
            | OpClass::Comment
            | OpClass::Tombstone
            | OpClass::Restore => true,
            // Editors mutate artifacts; they don't govern, don't issue
            // compaction checkpoints (host is pinned to the channel owner), and
            // don't redact — redaction is a governance act, and an editor who
            // wants something removed asks an owner.
            OpClass::Govern | OpClass::Compaction | OpClass::Redact => false,
        },
        Role::Commenter => matches!(op, OpClass::Comment),
        Role::Viewer | Role::Denied => false,
    }
}
