//! Roles, op kinds and artifact kinds of the signed-op envelope.

use serde::{Deserialize, Serialize};

/// Role presets a member holds; see `tilo_action_policy::roles`.
pub use tilo_action_policy::Role;

/// What a signed op carries. The action a change performs is named by the
/// op's claims ([`crate::claim::ChangeClaim`]), one per change; this only
/// says which kind of payload the op holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum OpKind {
    /// Changes to a record, each named by its claim.
    Change,
    /// A compaction checkpoint the room's governor publishes; payload is a
    /// [`crate::compaction::CompactionCheckpoint`] via
    /// [`crate::canonical::canonical_bytes`].
    Compaction,
    /// Cooperative cryptographic redaction the room's governor declares;
    /// payload is a [`crate::redaction::RedactRequest`] via
    /// [`crate::canonical::canonical_bytes`]. A redaction of a redaction is
    /// refused ([`crate::validator::ValidationError::RecursiveRedaction`]).
    Redaction,
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
