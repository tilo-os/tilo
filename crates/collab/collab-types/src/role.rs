//! Roles, op classes and artifact kinds of the signed-op envelope.

use serde::{Deserialize, Serialize};

/// Role presets a member holds; see `tilo_action_policy::roles`.
pub use tilo_action_policy::Role;

/// The semantic class of a single CRDT op, declared in the op envelope itself.
///
/// **Author-declared**, not validator-derived — a malicious client could
/// lie. The validator catches lies by re-checking the author's grants; the
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
    /// Cooperative cryptographic redaction. Only the channel **owner** mints
    /// a Redact op to declare "this earlier op is now
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
